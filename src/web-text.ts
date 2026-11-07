/** Readable text from an HTML page: title, then body text without scripts, styles or markup. */
export function pageText(html: string) {
  if (typeof DOMParser !== "undefined") {
    const doc = new DOMParser().parseFromString(html, "text/html");
    for (const node of doc.querySelectorAll("script, style, noscript, template, svg, iframe, nav, footer"))
      node.remove();
    for (const node of doc.querySelectorAll("br, p, div, li, h1, h2, h3, h4, h5, h6, tr, pre, section, article"))
      node.append("\n");
    const title = doc.title.trim();
    const body = collapse(doc.body?.textContent ?? "");
    return title ? `${title}\n\n${body}` : body;
  }
  return collapse(
    html
      .replace(/<(script|style|noscript|template|svg)[\s\S]*?<\/\1>/gi, "")
      .replace(/<(br|\/p|\/div|\/li|\/h\d|\/tr)\b[^>]*>/gi, "\n")
      .replace(/<[^>]+>/g, "")
      .replace(/&nbsp;/g, " ")
      .replace(/&lt;/g, "<")
      .replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, "&"),
  );
}

function collapse(value: string) {
  return value
    .split("\n")
    .map((line) => line.replace(/[ \t\r\f\v]+/g, " ").trim())
    .filter((line, i, lines) => line || (i > 0 && lines[i - 1]))
    .join("\n")
    .trim();
}
