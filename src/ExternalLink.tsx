import type { ComponentPropsWithoutRef } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

type Props = ComponentPropsWithoutRef<"a"> & { href: string; onOpenError: (error: unknown) => void };

/** Browser semantics in previews; the scoped native opener in the desktop App. */
export function ExternalLink({ href, onOpenError, onClick, children, ...props }: Props) {
  return (
    <a
      {...props}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(event) => {
        onClick?.(event);
        if (event.defaultPrevented || !isTauri()) return;
        event.preventDefault();
        void openUrl(href).catch(onOpenError);
      }}
    >
      {children}
    </a>
  );
}
