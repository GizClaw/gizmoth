/** Copy text in both browser and desktop WebViews, retaining the caller's focus. */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const previous = document.activeElement as HTMLElement | null;
    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.style.cssText = "position:fixed;opacity:0";
    document.body.append(textarea);
    try {
      textarea.select();
      if (!document.execCommand("copy")) throw new Error("Copy failed. Select the text and copy it manually.");
    } finally {
      textarea.remove();
      previous?.focus();
    }
  }
}
