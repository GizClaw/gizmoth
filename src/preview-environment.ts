/** Asset integrity checks need Web Crypto even when no firmware is running. */
export function modelPreviewIssue(): string | null {
  if (typeof globalThis.crypto?.subtle?.digest === "function") return null;
  return globalThis.isSecureContext
    ? "当前浏览器缺少模型校验所需的安全接口，请使用支持 Web Crypto 的浏览器。"
    : "此 HTTP 地址无法加载模型。请通过 HTTPS 访问，或使用这台 Mac 上的应用内预览。";
}
