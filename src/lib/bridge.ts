import { invoke } from "@tauri-apps/api/core";
import type { JournalSummary, ScanReport, Workspace } from "../types";

export async function listWorkspaces(): Promise<Workspace[]> {
  return invoke<Workspace[]>("list_workspaces");
}

export async function addWorkspace(name: string, path: string): Promise<Workspace> {
  return invoke<Workspace>("add_workspace", { name, path });
}

export async function removeWorkspace(id: string): Promise<void> {
  return invoke("remove_workspace", { id });
}

export async function scanWorkspace(id: string): Promise<ScanReport> {
  return invoke<ScanReport>("scan_workspace", { id });
}

export async function initializeIgnoreFile(id: string): Promise<boolean> {
  return invoke<boolean>("initialize_ignore_file", { id });
}

export async function getJournalSummary(id: string): Promise<JournalSummary> {
  return invoke<JournalSummary>("journal_summary", { id });
}

export async function listJournalSummaries(): Promise<JournalSummary[]> {
  return invoke<JournalSummary[]>("journal_summaries");
}
