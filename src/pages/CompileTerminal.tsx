import { useEffect, useMemo, useRef, useState, type CSSProperties, type RefObject } from "react";
import { Terminal, type IBufferCell } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

// A fixed terminal palette keeps build output legible in either application theme.
const palette = [
  "#1e1e1e",
  "#f48771",
  "#9ece6a",
  "#e5c07b",
  "#82aaff",
  "#c792ea",
  "#89ddff",
  "#d4d4d4",
  "#808080",
  "#ff9e9e",
  "#b9f27c",
  "#ffd68a",
  "#a6c8ff",
  "#ddb0ff",
  "#b2f0ff",
  "#ffffff",
];
const background = "#0c0c0c";
const foreground = "#d4d4d4";

function cellColor(cell: IBufferCell, side: "fg" | "bg"): string | undefined {
  const value = side === "fg" ? cell.getFgColor() : cell.getBgColor();
  if (side === "fg" ? cell.isFgRGB() : cell.isBgRGB()) return `#${value.toString(16).padStart(6, "0")}`;
  if (!(side === "fg" ? cell.isFgPalette() : cell.isBgPalette())) return undefined;
  if (value < 16) return palette[value];
  if (value >= 232)
    return `rgb(${Array(3)
      .fill(8 + (value - 232) * 10)
      .join(",")})`;
  const index = value - 16;
  const level = (n: number) => (n === 0 ? 0 : 55 + n * 40);
  return `rgb(${[Math.floor(index / 36), Math.floor(index / 6) % 6, index % 6].map(level).join(",")})`;
}

function cellStyle(cell: IBufferCell): CSSProperties {
  let color = cellColor(cell, "fg");
  let backgroundColor = cellColor(cell, "bg");
  if (cell.isInverse()) [color, backgroundColor] = [backgroundColor ?? background, color ?? foreground];
  return {
    color,
    backgroundColor,
    fontWeight: cell.isBold() ? 700 : undefined,
    fontStyle: cell.isItalic() ? "italic" : undefined,
    opacity: cell.isDim() ? 0.65 : undefined,
    visibility: cell.isInvisible() ? "hidden" : undefined,
    textDecoration:
      [cell.isUnderline() && "underline", cell.isStrikethrough() && "line-through"].filter(Boolean).join(" ") ||
      undefined,
  };
}

export type CompileTerminalHandle = { getText: () => string };

/** Read-only VT output. No command input, clipboard escape handler or link opener. */
export function CompileTerminal({
  text,
  query,
  follow,
  onFollow,
  empty,
  handle,
}: {
  text: string;
  query: string;
  follow: boolean;
  onFollow: (value: boolean) => void;
  empty: string;
  handle: RefObject<CompileTerminalHandle | null>;
}) {
  const host = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const previous = useRef("");
  const writing = useRef(false);
  const followRef = useRef(follow);
  const onFollowRef = useRef(onFollow);
  const filtered = useRef<HTMLPreElement>(null);
  const [revision, setRevision] = useState(0);
  followRef.current = follow;
  onFollowRef.current = onFollow;

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const view = new Terminal({
      allowProposedApi: false,
      disableStdin: true,
      linkHandler: { activate: () => {} },
      drawBoldTextInBrightColors: false,
      convertEol: true,
      cursorBlink: false,
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      fontSize: 12,
      lineHeight: 1,
      scrollback: 20000,
      screenReaderMode: true,
      theme: {
        background,
        foreground,
        cursor: background,
        selectionBackground: "#264f78",
        selectionInactiveBackground: "#264f78",
        black: palette[0],
        red: palette[1],
        green: palette[2],
        yellow: palette[3],
        blue: palette[4],
        magenta: palette[5],
        cyan: palette[6],
        white: palette[7],
        brightBlack: palette[8],
        brightRed: palette[9],
        brightGreen: palette[10],
        brightYellow: palette[11],
        brightBlue: palette[12],
        brightMagenta: palette[13],
        brightCyan: palette[14],
        brightWhite: palette[15],
      },
    });
    const fit = new FitAddon();
    view.loadAddon(fit);
    view.open(element);
    terminal.current = view;
    previous.current = "";
    handle.current = {
      getText: () => {
        const buffer = view.buffer.active;
        const lines: string[] = [];
        for (let row = 0; row < buffer.length; row++) {
          const line = buffer.getLine(row)!;
          const value = line.translateToString(!buffer.getLine(row + 1)?.isWrapped);
          if (line.isWrapped && lines.length) lines[lines.length - 1] += value;
          else lines.push(value);
        }
        return lines.join("\n").trimEnd();
      },
    };
    const resize = new ResizeObserver(() => {
      fit.fit();
      if (followRef.current) view.scrollToBottom();
      setRevision((value) => value + 1);
    });
    resize.observe(element);
    fit.fit();
    const scroll = view.onScroll(() => {
      if (!writing.current) onFollowRef.current(view.buffer.active.viewportY >= view.buffer.active.baseY);
    });
    return () => {
      resize.disconnect();
      scroll.dispose();
      handle.current = null;
      terminal.current = null;
      view.dispose();
    };
  }, [handle]);

  useEffect(() => {
    const view = terminal.current;
    if (!view) return;
    const appended = text.startsWith(previous.current);
    const delta = appended ? text.slice(previous.current.length) : text;
    previous.current = text;
    if (!delta && appended) return;
    const position = view.buffer.active.viewportY;
    writing.current = true;
    if (!appended) view.reset();
    view.write(delta, () => {
      if (terminal.current !== view) return;
      if (followRef.current) view.scrollToBottom();
      else view.scrollToLine(position);
      writing.current = false;
      setRevision((value) => value + 1);
    });
  }, [text]);

  // Filtering uses the parsed screen buffer so control sequences never become text,
  // and matches keep the same ANSI colors as the terminal, including 256/RGB colors.
  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const view = terminal.current;
    if (!needle || !view) return [];
    const lines: { text: string; rows: number[] }[] = [];
    const buffer = view.buffer.active;
    for (let row = 0; row < buffer.length; row++) {
      const line = buffer.getLine(row)!;
      if (!line.isWrapped || !lines.length) lines.push({ text: "", rows: [] });
      const entry = lines[lines.length - 1];
      entry.text += line.translateToString(!buffer.getLine(row + 1)?.isWrapped);
      entry.rows.push(row);
    }
    return lines.filter((line) => line.text.toLowerCase().includes(needle)).map((entry) => {
      const runs: { text: string; style: CSSProperties }[] = [];
      let lastKey = "";
      for (const row of entry.rows) {
        const line = buffer.getLine(row)!;
        // Keep full wrapped rows, but don't pad the end of a logical line.
        const end = buffer.getLine(row + 1)?.isWrapped ? line.length : line.translateToString(true).length;
        let width = 0;
        for (let column = 0; column < line.length && width < end; column++) {
          const cell = line.getCell(column);
          if (!cell || cell.getWidth() === 0) continue;
          const style = cellStyle(cell);
          const key = JSON.stringify(style);
          const chars = cell.getChars() || " ";
          if (key === lastKey && runs.length) runs[runs.length - 1].text += chars;
          else runs.push({ text: chars, style });
          lastKey = key;
          width += chars.length;
        }
      }
      return { text: entry.text, runs };
    });
  }, [query, revision]);

  useEffect(() => {
    if (!follow) return;
    terminal.current?.scrollToBottom();
    const element = filtered.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [follow, matches]);

  const filtering = Boolean(query.trim());
  return (
    <div className="compile-terminal">
      <div
        ref={host}
        className="compile-terminal-screen"
        style={{ visibility: filtering || !text ? "hidden" : undefined }}
        aria-hidden={filtering || !text}
      />
      {filtering && (
        <pre
          ref={filtered}
          className="compile-terminal-filter"
          tabIndex={0}
          onScroll={(event) => {
            const element = event.currentTarget;
            onFollow(element.scrollHeight - element.scrollTop - element.clientHeight < 24);
          }}
        >
          {matches.length
            ? matches.map((line, index) => (
                <span key={index}>
                  {line.runs.map((run, i) => (
                    <span key={i} style={run.style}>
                      {run.text}
                    </span>
                  ))}
                  {"\n"}
                </span>
              ))
            : empty}
        </pre>
      )}
      {!text && !filtering && <pre className="compile-terminal-empty">{empty}</pre>}
    </div>
  );
}
