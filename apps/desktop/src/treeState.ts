import { ref } from "vue";

/** Where a dragged node would land, relative to the node under the pointer. */
export type DropPosition = "before" | "after" | "into";

/** State shared by every level of the (recursive) tree view. */
export const dragging = ref<{ id: string; folder: boolean } | null>(null);
export const dropHint = ref<{ id: string; position: DropPosition } | null>(null);
export const renamingId = ref("");
export const draftName = ref("");

/** Starts renaming a node in place (from a double click, F2 or the context menu). */
export function startRenaming(id: string, name: string) {
  draftName.value = name;
  renamingId.value = id;
}

/** The folder that holds a node ("" = top level). */
export function parentOf(id: string): string {
  const slash = id.lastIndexOf("/");
  return slash < 0 ? "" : id.slice(0, slash);
}

/** True when `id` is `folder` or inside it. */
export function within(id: string, folder: string): boolean {
  return id === folder || id.startsWith(`${folder}/`);
}
