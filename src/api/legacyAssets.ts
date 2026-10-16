import { invoke } from "./client";

export interface LegacyHashMatch {
  assetId: number;
  fileName: string;
  favorite: number;
  rating: number;
  contentSha256: string;
  tags: string[];
}

export interface LegacyAsset {
  assetId: number;
  filePath: string;
  fileName: string;
  fileSize: number;
  mimeType: string;
  modifiedAt: number;
  hash: string | null;
  status: string;
  reason: string;
  favorite: number;
  rating: number;
  activeResolutionId: string | null;
  activeResolutionAction: "copy_back" | "unlink" | null;
  activeResolutionState: string | null;
  resolutionErrorCode: string | null;
  matchingAssets: LegacyHashMatch[];
}

export interface LegacyAssetPage {
  libraryHealth: string;
  canResolve: boolean;
  rootPath: string | null;
  items: LegacyAsset[];
  nextAfterAssetId: number | null;
}

export interface LegacyResolutionPreview {
  resolutionId: string;
  asset: LegacyAsset;
  action: "copy_back" | "unlink";
  matchingAssets: LegacyHashMatch[];
  keepSeparateRequired: boolean;
  expiresAt: number;
}

export interface LegacyResolutionResult {
  resolutionId: string;
  assetId: number;
  action: "copy_back" | "unlink";
  state: string;
  targetPath: string | null;
  matchingAssets: LegacyHashMatch[];
  errorCode: string | null;
  expiresAt: number;
}

export function listLegacyAssets(afterAssetId = 0, limit = 50): Promise<LegacyAssetPage> {
  return invoke("list_legacy_assets", { afterAssetId, limit });
}

export function listUnlinkedLegacyAssets(afterAssetId = 0, limit = 50): Promise<LegacyAsset[]> {
  return invoke("list_unlinked_legacy_assets", { afterAssetId, limit });
}

export function prepareLegacyUnlink(assetId: number): Promise<LegacyResolutionPreview> {
  return invoke("prepare_legacy_unlink", { assetId });
}

export function prepareLegacyCopyBack(assetId: number): Promise<LegacyResolutionPreview | null> {
  return invoke("prepare_legacy_copy_back", { assetId });
}

export function cancelLegacyResolution(resolutionId: string): Promise<void> {
  return invoke("cancel_legacy_asset_resolution", { resolutionId });
}

export function confirmLegacyResolution(resolutionId: string): Promise<LegacyResolutionResult> {
  return invoke("confirm_legacy_asset_resolution", { resolutionId });
}

export function reopenUnlinkedLegacyAsset(assetId: number): Promise<void> {
  return invoke("reopen_unlinked_legacy_asset", { assetId });
}
