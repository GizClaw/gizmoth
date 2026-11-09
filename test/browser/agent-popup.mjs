import { bridge, demo } from "/src/bridge.ts";
import { createSimulatorTools } from "/src/agent-simulator.ts";
import { simulatorProfiles } from "/src/simulator-profiles.ts";
import { closeSimulator } from "/src/simulator-window.ts";

const output = document.querySelector("#result");
const button = document.querySelector("#start");
const dialog = document.querySelector("#activation-dialog");
if (!demo) throw new Error("Open this fixture with ?demo=1.");
// The browser simulator opens the current app entry path, not this fixture's HTML path.
history.replaceState(null, "", "/?demo=1");
const project = (await bridge.projects())[0];
if (!project) throw new Error("Demo project is missing.");
button.disabled = false;
output.textContent = "Ready. Start the delayed run, then click Open in the confirmation.";
button.onclick = async () => {
  button.disabled = true;
  const signal = new AbortController();
  const profile = await simulatorProfiles.create("Popup regression", "h106-tiga");
  const originalOpen = window.open;
  const observed = { confirmed: false, active: false, opened: false };
  const record = (stage, message) =>
    localStorage.setItem("gizmoth.test.agent-popup", JSON.stringify({ ...observed, stage, message }));
  record("preparing");
  window.open = function (...args) {
    observed.active = navigator.userActivation?.isActive === true;
    const popup = originalOpen.apply(window, args);
    observed.opened = !!popup;
    record("opened");
    // This fixture tests popup admission only; stop before starting network/media firmware.
    queueMicrotask(() => signal.abort());
    return popup;
  };
  try {
    output.textContent = "Waiting for the original transient user activation to expire…";
    await new Promise((resolve) => setTimeout(resolve, 6200));
    const tools = createSimulatorTools({
      projectId: project.id,
      device: () => undefined,
      onFilesChanged() {},
      permissions: () => ({ files: "read", run: "allow", web: "allow" }),
      confirm: (title, body, labels) =>
        new Promise((resolve) => {
          document.querySelector("#dialog-title").textContent = title;
          document.querySelector("#dialog-body").textContent = body;
          document.querySelector("#allow-open").textContent = labels?.confirm ?? "Allow";
          document.querySelector("#allow-open").onclick = () => {
            observed.confirmed = true;
            dialog.close();
            resolve(true);
          };
          document.querySelector("#decline-open").onclick = () => {
            dialog.close();
            resolve(false);
          };
          dialog.showModal();
        }),
    });
    try {
      await tools
        .find((tool) => tool.name === "run_in_simulator")
        .execute("popup-regression", { profileId: profile.id }, signal.signal);
    } catch (error) {
      if (!signal.signal.aborted) throw error;
    }
    if (!observed.confirmed || !observed.active || !observed.opened)
      throw new Error(`Popup admission failed: ${JSON.stringify(observed)}`);
    record("pass");
    output.textContent =
      "PASS: the popup opened with active user activation after delayed allow-mode preparation. The test canceled before firmware boot.";
  } catch (error) {
    record("fail", error.message);
    output.textContent = `FAIL: ${error.message}`;
    console.error(error);
  } finally {
    window.open = originalOpen;
    closeSimulator(profile.id);
    await simulatorProfiles.delete(profile.id);
  }
};
