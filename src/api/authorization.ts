/** 目录授权和受控资源句柄（对应 commands/authorization_cmd.rs）。 */
import { invoke } from "./client";

export type DirectoryPurpose = "import_source" | "export_destination";

export interface DirectoryGrantAccess {
  read: boolean;
  write: boolean;
  export: boolean;
}

export interface DirectoryGrant {
  grantRef: string;
  directoryRef?: string;
  displayName: string;
  displayPath: string;
  purpose: DirectoryPurpose;
  recursive: boolean;
  remembered: boolean;
  expiresAtUnixSeconds?: number;
  available: boolean;
  access: DirectoryGrantAccess;
}

export interface AuthorizedDirectoryEntry {
  name: string;
  kind: "file";
  size: number;
  modifiedAt: number;
  entryRef: string;
}

export interface DirectoryPage {
  items: AuthorizedDirectoryEntry[];
  nextCursor?: number;
  truncated: boolean;
}

/** 原生选择器由 Rust 宿主打开；前端不能传入任意目录路径。 */
export function pickDirectoryGrant(
  purpose: DirectoryPurpose,
  remembered: boolean,
): Promise<DirectoryGrant | null> {
  return invoke<DirectoryGrant | null>("pick_directory_grant", {
    purpose,
    remembered,
  });
}

export function listDirectoryGrants(): Promise<DirectoryGrant[]> {
  return invoke<DirectoryGrant[]>("list_directory_grants");
}

export function reissueDirectoryGrant(grantRef: string): Promise<DirectoryGrant> {
  return invoke<DirectoryGrant>("reissue_directory_grant", { grantRef });
}

export function revokeDirectoryGrant(grantRef: string): Promise<void> {
  return invoke<void>("revoke_directory_grant", { grantRef });
}

export function revokeAllDirectoryGrants(): Promise<void> {
  return invoke<void>("revoke_all_directory_grants");
}

export function pageAuthorizedDirectory(
  directoryRef: string,
  cursor?: number,
  limit = 100,
): Promise<DirectoryPage> {
  return invoke<DirectoryPage>("page_authorized_directory", {
    directoryRef,
    cursor,
    limit,
  });
}
