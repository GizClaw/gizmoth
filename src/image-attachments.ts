/** Images the user attaches to a prompt: accepted types, size limits and history budgeting. */

export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
export const MAX_IMAGES = 4;
/** Longest side sent to the model; larger images are scaled down before sending. */
export const MAX_IMAGE_SIDE = 1568;
/** Raw bytes kept as-is when already small enough (≈5 MB once base64 encoded). */
const MAX_IMAGE_BYTES = 3_750_000;
/** Files larger than this are refused before decoding. */
const MAX_SOURCE_BYTES = 30 * 1024 * 1024;
/** Saved chat history must stay under the native 8 MiB limit. */
export const HISTORY_BUDGET = 7 * 1024 * 1024;
export const DROPPED_IMAGE_TEXT = "（图片较大，未保存在对话记录中）";

export interface Attachment {
  id: string;
  name: string;
  mimeType: string;
  data: string;
  /** Source label for generated names; actual filenames stay untouched. */
  nameKey?: string;
  nameValues?: Record<string, string | number>;
}

export function isAcceptedImage(type: string) {
  return (IMAGE_TYPES as readonly string[]).includes(type);
}

/** Scale (width, height) to fit within `max` on the longest side, never upscaling. */
export function fitWithin(width: number, height: number, max = MAX_IMAGE_SIDE) {
  const scale = Math.min(1, max / Math.max(width, height, 1));
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

function toBase64(bytes: Uint8Array) {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

/** Read an image file, scaling and re-encoding it to JPEG when it is too large to send as-is. */
export async function prepareImage(
  file: File,
  translate?: (message: string, values?: Record<string, string | number>) => string,
): Promise<Attachment> {
  if (!isAcceptedImage(file.type))
    throw new Error(
      translate?.("不支持 {name}，只能添加 PNG、JPEG、GIF 或 WebP 图片。", { name: file.name || translate("这个文件") }) ??
        `不支持 ${file.name || "这个文件"}，只能添加 PNG、JPEG、GIF 或 WebP 图片。`,
    );
  if (file.size > MAX_SOURCE_BYTES)
    throw new Error(
      translate?.("{name} 超过 30 MB，换一张小一点的图片。", { name: file.name || translate("图片") }) ??
        `${file.name || "图片"} 超过 30 MB，换一张小一点的图片。`,
    );
  const id = crypto.randomUUID();
  const name = file.name || "粘贴的图片";
  const generatedName = !file.name && translate ? { nameKey: "粘贴的图片" } : {};
  const displayName = file.name || translate?.("粘贴的图片") || name;
  const bitmap = await createImageBitmap(file).catch(() => {
    throw new Error(translate?.("{name} 无法读取，文件可能已损坏。", { name: displayName }) ?? `${name} 无法读取，文件可能已损坏。`);
  });
  try {
    const size = fitWithin(bitmap.width, bitmap.height);
    const unchanged = size.width === bitmap.width && size.height === bitmap.height;
    if (unchanged && file.size <= MAX_IMAGE_BYTES)
      return { id, name, ...generatedName, mimeType: file.type, data: toBase64(new Uint8Array(await file.arrayBuffer())) };
    const canvas = new OffscreenCanvas(size.width, size.height);
    const context = canvas.getContext("2d");
    if (!context) throw new Error(translate?.("{name} 无法处理。", { name: displayName }) ?? `${name} 无法处理。`);
    // JPEG has no alpha: flatten transparent areas onto white.
    context.fillStyle = "#fff";
    context.fillRect(0, 0, size.width, size.height);
    context.drawImage(bitmap, 0, 0, size.width, size.height);
    const blob = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.85 });
    return { id, name, ...generatedName, mimeType: "image/jpeg", data: toBase64(new Uint8Array(await blob.arrayBuffer())) };
  } finally {
    bitmap.close();
  }
}

type Block = { type?: string; data?: string };
type Message = { role?: string; content?: unknown };

/**
 * Keep saved history under `budget` bytes of JSON by replacing image data with a
 * short note, oldest images first. Messages are copied, never mutated.
 */
export function fitHistory<T>(messages: T[], budget = HISTORY_BUDGET): T[] {
  let size = JSON.stringify(messages).length;
  if (size <= budget) return messages;
  const out = messages.map((message) => {
    const content = (message as Message).content;
    return Array.isArray(content) ? ({ ...message, content: [...content] } as T) : message;
  });
  for (const message of out) {
    const content = (message as Message).content;
    if (!Array.isArray(content)) continue;
    for (let i = 0; i < content.length && size > budget; i++) {
      const block = content[i] as Block;
      if (block?.type !== "image" || typeof block.data !== "string") continue;
      const replacement = { type: "text", text: DROPPED_IMAGE_TEXT };
      size -= JSON.stringify(block).length - JSON.stringify(replacement).length;
      content[i] = replacement;
    }
    if (size <= budget) break;
  }
  return out;
}
