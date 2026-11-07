import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createUpdater, type UpdateCheckState, type UpdateState } from "./updater";
import { updateDriver } from "./update-driver";
import { prepareAppUpdate } from "./update-preparation";

const UpdatesContext = createContext<{
  state: UpdateState;
  checkState: UpdateCheckState;
  supported: boolean | null;
  check: () => void;
  activate: () => void;
} | null>(null);

/** One updater for the main window, shared by the titlebar and About page. */
export function UpdatesProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<UpdateState>({ phase: "hidden" });
  const [checkState, setCheckState] = useState<UpdateCheckState>({ phase: "idle" });
  const [supported, setSupported] = useState<boolean | null>(null);
  const updater = useRef<ReturnType<typeof createUpdater> | null>(null);
  useEffect(() => {
    const driver = updateDriver();
    setSupported(!!driver);
    if (!driver) return;
    const controller = createUpdater(driver, setState, prepareAppUpdate, setCheckState);
    updater.current = controller;
    void controller.check();
    const timer = window.setInterval(() => void controller.check(), 60 * 60 * 1000);
    const online = () => void controller.check();
    window.addEventListener("online", online);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("online", online);
      controller.dispose();
      updater.current = null;
    };
  }, []);
  return (
    <UpdatesContext.Provider
      value={{
        state,
        checkState,
        supported,
        check: () => void updater.current?.check(),
        activate: () => void updater.current?.activate(),
      }}
    >
      {children}
    </UpdatesContext.Provider>
  );
}

export function useUpdates() {
  const value = useContext(UpdatesContext);
  if (!value) throw new Error("UpdatesProvider is missing");
  return value;
}
