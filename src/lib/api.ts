/**
 * Typed wrappers for every Tauri command in the shared contract,
 * plus a polled Status store.
 *
 * Commands are snake_case in Rust and invoked with camelCase arg keys
 * (Tauri v2 auto-converts). All commands return Result<_, String>, so
 * a rejected invoke gives us a plain string message.
 */
import { invoke } from "@tauri-apps/api/core";
import { writable, type Writable } from "svelte/store";

/* ── contract types ───────────────────────────────────────────── */

export type Phase = "fresh" | "setting_up" | "running" | "stopped" | "error";

export type SetupStage = "starting_services" | "minting_address" | "ready";

export type ServiceState = "starting" | "healthy" | "stopped" | "error";

export interface Service {
  name: "homeserver" | "tor";
  state: ServiceState;
}

export interface Status {
  phase: Phase;
  onion: string | null;
  demo_mode: boolean;
  setup_stage: SetupStage | null;
  services: Service[];
  people_count: number;
  paired_count: number;
  box_name: string;
}

export interface RecoveryKit {
  phrase: string;
  onion: string | null;
  created: string;
  box_name: string;
}

export interface ConnectQr {
  /** "pureprivacy://connect?hs=<onion>&user=<username>&token=<hex>" */
  payload: string;
  /** A complete inline <svg> QR code. */
  svg: string;
}

/* ── environment guard ────────────────────────────────────────── */

/** True when running inside a Tauri webview (invoke is available). */
export function hasTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/* ── command wrappers ─────────────────────────────────────────── */

export function getStatus(): Promise<Status> {
  return invoke<Status>("get_status");
}

export function suggestPassword(): Promise<string> {
  return invoke<string>("suggest_password");
}

export function beginSetup(args: {
  boxName: string;
  username: string;
  password: string;
}): Promise<null> {
  return invoke<null>("begin_setup", args);
}

export function getRecoveryKit(): Promise<RecoveryKit> {
  return invoke<RecoveryKit>("get_recovery_kit");
}

export function confirmRecoveryWord(args: {
  /** 0-based index into the phrase words. */
  index: number;
  word: string;
}): Promise<boolean> {
  return invoke<boolean>("confirm_recovery_word", args);
}

/** Writes a printable HTML kit into Downloads; returns the absolute path. */
export function saveRecoveryKitHtml(): Promise<string> {
  return invoke<string>("save_recovery_kit_html");
}

export function getConnectQr(): Promise<ConnectQr> {
  return invoke<ConnectQr>("get_connect_qr");
}

export function stopBox(): Promise<null> {
  return invoke<null>("stop_box");
}

export function startBox(): Promise<null> {
  return invoke<null>("start_box");
}

export interface LegacyInstall {
  present: boolean;
  containers: string[];
}

/** Detect a running v0.1 Docker appliance so we never silently orphan it. */
export function detectLegacyInstall(): Promise<LegacyInstall> {
  return invoke<LegacyInstall>("detect_legacy_install");
}

export interface JoinInfo {
  onion: string;
  join_token: string;
  /** QR encoding pureprivacy://join?hs=…&token=… */
  svg: string;
}

/** What a new person needs to join this box (People → Add a person). */
export function getJoinInfo(): Promise<JoinInfo> {
  return invoke<JoinInfo>("get_join_info");
}

export interface AppInfo {
  version: string;
  data_dir: string;
  demo_mode: boolean;
}

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

/** Wipe the box and return to fresh setup. Destructive — confirm first. */
export function resetBox(): Promise<null> {
  return invoke<null>("reset_box");
}

/* ── status store, polled every 1.5 s ─────────────────────────── */

export const status: Writable<Status | null> = writable(null);

let pollTimer: ReturnType<typeof setInterval> | null = null;

export function startStatusPolling(intervalMs = 1500): void {
  if (pollTimer !== null) return;
  const tick = async () => {
    if (!hasTauri()) return; // plain-browser context: nothing to poll
    try {
      status.set(await getStatus());
    } catch {
      // box may be mid-restart — keep the last known status, try again
    }
  };
  void tick();
  pollTimer = setInterval(tick, intervalMs);
}

export function stopStatusPolling(): void {
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

/* ── small shared helper ──────────────────────────────────────── */

/** Copy text to the clipboard; returns false if the platform refused. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      return ok;
    } catch {
      return false;
    }
  }
}
