/** 会话索引与持久工作台草稿（对应 commands/conversations_cmd.rs）。 */
import { invoke } from "./client";

export type ConversationStatus = "active" | "archived";
export type DraftKind = "import" | "tagging" | "export" | "workbench" | "assistant";
export type ConversationLinkKind = "plan" | "operation" | "batch";

export interface ConversationSummary {
  conversationRef: string;
  title: string;
  status: ConversationStatus;
  createdAtUnixSeconds: number;
  updatedAtUnixSeconds: number;
  archivedAtUnixSeconds?: number;
}

export interface ConversationDraft {
  kind: DraftKind;
  payload: Record<string, unknown>;
  revision: number;
  updatedAtUnixSeconds: number;
}

export interface ConversationAttachment {
  attachmentRef: string;
  assetId: number;
  displayName: string;
  /** false 表示素材缺失或未核验；UI 必须明示，不能静默重新授权。 */
  available: boolean;
}

export interface ConversationLink {
  kind: ConversationLinkKind;
  reference: string;
}

export interface ConversationView {
  conversationRef: string;
  title: string;
  status: ConversationStatus;
  createdAtUnixSeconds: number;
  updatedAtUnixSeconds: number;
  drafts: ConversationDraft[];
  attachments: ConversationAttachment[];
  links: ConversationLink[];
}

export function createConversation(title: string): Promise<ConversationSummary> {
  return invoke<ConversationSummary>("create_conversation", { title });
}

export function listConversations(
  includeArchived = false,
  query?: string,
): Promise<ConversationSummary[]> {
  return invoke<ConversationSummary[]>("list_conversations", {
    includeArchived,
    query,
  });
}

export function getConversation(conversationRef: string): Promise<ConversationView> {
  return invoke<ConversationView>("get_conversation", { conversationRef });
}

export function renameConversation(
  conversationRef: string,
  title: string,
): Promise<ConversationSummary> {
  return invoke<ConversationSummary>("rename_conversation", {
    conversationRef,
    title,
  });
}

export function setConversationArchived(
  conversationRef: string,
  archived: boolean,
): Promise<ConversationSummary> {
  return invoke<ConversationSummary>("set_conversation_archived", {
    conversationRef,
    archived,
  });
}

export function deleteConversation(conversationRef: string): Promise<void> {
  return invoke<void>("delete_conversation", { conversationRef });
}

export function saveConversationDraft(
  conversationRef: string,
  kind: DraftKind,
  payload: Record<string, unknown>,
): Promise<ConversationDraft> {
  return invoke<ConversationDraft>("save_conversation_draft", {
    conversationRef,
    kind,
    payload,
  });
}

export function attachConversationAsset(
  conversationRef: string,
  assetId: number,
  displayName: string,
): Promise<ConversationAttachment> {
  return invoke<ConversationAttachment>("attach_conversation_asset", {
    conversationRef,
    assetId,
    displayName,
  });
}

export function linkConversation(
  conversationRef: string,
  kind: ConversationLinkKind,
  reference: string,
): Promise<void> {
  return invoke<void>("link_conversation", { conversationRef, kind, reference });
}
