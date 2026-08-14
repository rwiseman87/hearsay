import { invoke, isTauri } from "@tauri-apps/api/core";

// Post a native OS "still recording?" notification through the desktop shell — the follow-on to the
// in-app banner for a user who has switched away from the window and would not see it. Desktop-only
// and best-effort: a no-op in the browser (dev) or while the window is focused (the banner suffices),
// and any IPC/permission failure is swallowed so it never disrupts the meeting. The shell posts it
// via a granted custom command (see `notify_still_recording`), not the JS notification plugin.
export function notifyStillRecordingIfAway(silentMinutes: number): void {
  if (!isTauri()) return;
  if (typeof document !== "undefined" && document.hasFocus()) return;
  const minutes = Math.max(1, silentMinutes);
  const body =
    `No one has spoken for about ${minutes} minute${minutes === 1 ? "" : "s"}. ` +
    "Hearsay is still recording — it will stop automatically if the silence continues.";
  void invoke("notify_still_recording", { title: "Hearsay is still recording", body }).catch(() => {
    // Notification unavailable (permission denied, IPC failure) — the in-app banner still shows.
  });
}
