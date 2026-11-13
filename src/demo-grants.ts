import type { PermissionState, SystemPermission, SystemPermissions } from "./grants";

// Synthetic permissions only. Browser previews never invoke an OS prompt or send a notification.
const fixture = typeof location !== "undefined" ? new URLSearchParams(location.search).get("grants") : null;
const states: SystemPermissions = {
  notifications: fixture === "denied" ? "denied" : "prompt",
  bluetooth: fixture === "denied" ? "denied" : "prompt",
  localNetwork: "unknown",
};
let serialTasks = true;
export function demoGrants(command: string, args: Record<string, unknown>): unknown {
  if (fixture === "error") throw new Error("无法读取系统授权，请刷新重试。");
  switch (command) {
    case "system_permissions":
      return { ...states };
    case "notification_settings":
      return { serialTasks };
    case "notification_settings_set":
      serialTasks = args.serialTasks === true;
      return;
    case "system_permission_request": {
      const permission = args.permission as Exclude<SystemPermission, "localNetwork">;
      if (states[permission] === "prompt") states[permission] = "granted";
      return states[permission] satisfies PermissionState;
    }
    case "system_permission_settings":
      return;
    case "notification_test":
      if (states.notifications !== "granted") throw new Error("请先允许系统通知。");
      return;
    default:
      throw new Error(`Unknown demo permission command: ${command}`);
  }
}
