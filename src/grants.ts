import { call } from "./bridge";

export type PermissionState = "granted" | "denied" | "prompt" | "unknown" | "unavailable";
export type SystemPermission = "notifications" | "bluetooth" | "localNetwork";
export type SystemPermissions = Record<SystemPermission, PermissionState>;
export interface NotificationSettings {
  serialTasks: boolean;
}

export const grants = {
  permissions: () => call<SystemPermissions>("system_permissions"),
  request: (permission: Exclude<SystemPermission, "localNetwork">) =>
    call<PermissionState>("system_permission_request", { permission }),
  openSettings: (permission: SystemPermission) => call<void>("system_permission_settings", { permission }),
  notifications: () => call<NotificationSettings>("notification_settings"),
  setNotifications: (serialTasks: boolean) => call<void>("notification_settings_set", { serialTasks }),
  testNotification: () => call<void>("notification_test"),
};
