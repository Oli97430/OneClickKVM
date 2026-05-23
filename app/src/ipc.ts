// Bindings TypeScript pour les commands Tauri exposees par le backend Rust
// (src-tauri/src/commands.rs). Les types DTO miroitent ceux declares dans
// le crate `okvm-ipc`.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// DTOs (cf. okvm-ipc)
// ---------------------------------------------------------------------------

export interface AppStatus {
  self_identity: number[]; // [u8; 32] cote Rust, vehicule comme tableau d'octets
  self_fingerprint: number[]; // [u8; 16]
  self_hostname: string;
  connected_peers: number;
  listening: boolean;
}

export interface ScreenRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface GridPeerView {
  name: string;
  hotkey: number | null;
  bbox: ScreenRect;
  is_self: boolean;
}

export async function getGrid(): Promise<GridPeerView[]> {
  return await invoke("get_grid");
}

export async function sendFiles(deviceId: number[], files: string[]): Promise<void> {
  await invoke("send_files", { deviceId, files });
}

export async function getInboxDir(): Promise<string> {
  return await invoke("get_inbox_dir");
}

export async function startAudioShare(): Promise<void> {
  await invoke("start_audio_share");
}

export async function stopAudioShare(): Promise<void> {
  await invoke("stop_audio_share");
}

export async function isAudioSharing(): Promise<boolean> {
  return await invoke("is_audio_sharing");
}

export async function startVideoShare(): Promise<void> {
  await invoke("start_video_share");
}

export async function stopVideoShare(): Promise<void> {
  await invoke("stop_video_share");
}

export async function isVideoSharing(): Promise<boolean> {
  return await invoke("is_video_sharing");
}

export interface VideoFrameEvent {
  device_id: number[];
  seq: number;
  jpeg_b64: string;
}

export interface VideoStartEvent {
  device_id: number[];
  width: number;
  height: number;
  fps: number;
}

export async function onVideoFrame(
  handler: (e: VideoFrameEvent) => void,
): Promise<UnlistenFn> {
  return await listen<VideoFrameEvent>("okvm://video-frame", (event) => {
    handler(event.payload);
  });
}

export async function onVideoStart(
  handler: (e: VideoStartEvent) => void,
): Promise<UnlistenFn> {
  return await listen<VideoStartEvent>("okvm://video-stream-start", (event) => {
    handler(event.payload);
  });
}

export async function onVideoStop(
  handler: (e: { device_id: number[] }) => void,
): Promise<UnlistenFn> {
  return await listen<{ device_id: number[] }>("okvm://video-stream-stop", (event) => {
    handler(event.payload);
  });
}

export type H264BackendChoice = "Openh264" | "MediaFoundation";

export interface AppConfig {
  language: string;
  theme: "System" | "Light" | "Dark";
  bind_addr: string;
  start_minimized: boolean;
  autostart: boolean;
  discovery_broadcast: boolean;
  discovery_mdns: boolean;
  redact_logs: boolean;
  h264_backend: H264BackendChoice;
  video_screen_idx: number;
}

export interface ScreenView {
  index: number;
  is_primary: boolean;
  width_px: number;
  height_px: number;
  origin_x: number;
  origin_y: number;
}

export async function listLocalScreens(): Promise<ScreenView[]> {
  return await invoke("list_local_screens");
}

export interface H264EncoderView {
  friendly_name: string;
  is_hardware: boolean;
  is_async_mode: boolean;
}

export interface AboutInfo {
  app_name: string;
  version: string;
  rust_target: string;
  license: string;
  self_fingerprint: number[];
  self_hostname: string;
  inbox_dir: string;
  tcp_port: number;
  h264_encoders: H264EncoderView[];
  has_hardware_h264: boolean;
  /** Backend MFT choisi par new_best() — ex: "software (Microsoft …)" */
  mft_backend_active: string;
}

export async function getAppConfig(): Promise<AppConfig> {
  return await invoke("get_app_config");
}

export async function setAppConfig(cfg: AppConfig): Promise<void> {
  await invoke("set_app_config", { cfg });
}

export async function resetAllSettings(): Promise<void> {
  await invoke("reset_all_settings");
}

/** Ouvre `%APPDATA%/OneClickKVM/` dans l'explorateur Windows. */
export async function openConfigDir(): Promise<void> {
  await invoke("open_config_dir");
}

/** Ouvre le dossier de réception (`Documents/OneClickKVM/Inbox/`). */
export async function openInboxDir(): Promise<void> {
  await invoke("open_inbox_dir");
}

export interface PairingModeView {
  active: boolean;
  pin: string | null;
  expires_at_ms: number | null;
}

/**
 * Active le mode d'appairage : génère un PIN à 6 chiffres affiché à
 * l'utilisateur, valide pendant `durationSecs` (clamped [10, 600]).
 */
export async function startPairingMode(durationSecs: number = 60): Promise<PairingModeView> {
  return await invoke("start_pairing_mode", { durationSecs });
}

export async function stopPairingMode(): Promise<void> {
  await invoke("stop_pairing_mode");
}

export async function getPairingModeStatus(): Promise<PairingModeView> {
  return await invoke("get_pairing_mode_status");
}

export async function getAboutInfo(): Promise<AboutInfo> {
  return await invoke("get_about_info");
}

export interface PeerView {
  device_id: number[];
  fingerprint: number[];
  name: string;
  paired: boolean;
  online: boolean;
  discovered: boolean;
  last_addr: string | null;
}

export interface PairRequest {
  address: string;
  pin: string;
}

export type PairResult =
  | { kind: "success"; device_id: number[]; fingerprint: number[]; name: string }
  | { kind: "failure"; reason: string };

export type NotificationLevel = "info" | "success" | "warn" | "error";

export interface TransferProgressView {
  transfer_id: string;
  direction: "outbound" | "inbound";
  peer_name: string;
  current_file: string;
  bytes_done: number;
  bytes_total: number;
  state: "running" | "done" | "error" | "cancelled";
  error: string | null;
}

export type BackendEvent =
  | { type: "status_changed"; status: AppStatus }
  | { type: "peer_discovered"; peer: PeerView }
  | { type: "peer_connected"; device_id: number[] }
  | { type: "peer_disconnected"; device_id: number[]; reason: string }
  | {
      type: "notification";
      level: NotificationLevel;
      title: string;
      body: string;
    }
  | { type: "confirmation_requested"; request_id: string; prompt: string }
  | { type: "transfer_progress"; progress: TransferProgressView };

// ---------------------------------------------------------------------------
// Commands (Tauri ↔ Rust)
// ---------------------------------------------------------------------------

export async function getAppStatus(): Promise<AppStatus> {
  return await invoke("get_app_status");
}

export async function listPeers(): Promise<PeerView[]> {
  return await invoke("list_peers");
}

export async function startListening(): Promise<void> {
  await invoke("start_listening");
}

export async function stopListening(): Promise<void> {
  await invoke("stop_listening");
}

export async function pairWithPeer(req: PairRequest): Promise<PairResult> {
  return await invoke("pair_with_peer", { req });
}

export async function unpairPeer(deviceId: number[]): Promise<void> {
  await invoke("unpair_peer", { deviceId });
}

export async function becomeMaster(): Promise<void> {
  await invoke("become_master");
}

export async function stopMaster(): Promise<void> {
  await invoke("stop_master");
}

// ---------------------------------------------------------------------------
// Events (Rust → Tauri)
// ---------------------------------------------------------------------------

export async function onBackendEvent(
  handler: (event: BackendEvent) => void,
): Promise<UnlistenFn> {
  return await listen<BackendEvent>("okvm://backend-event", (event) => {
    handler(event.payload);
  });
}

// ---------------------------------------------------------------------------
// Utilitaires
// ---------------------------------------------------------------------------

export function fingerprintToString(fp: number[]): string {
  // 16 octets → 8 mots de 4 hex separes par espaces
  const out: string[] = [];
  for (let i = 0; i < fp.length; i += 2) {
    const hi = fp[i].toString(16).padStart(2, "0");
    const lo = (fp[i + 1] ?? 0).toString(16).padStart(2, "0");
    out.push(hi + lo);
  }
  return out.join(" ");
}
