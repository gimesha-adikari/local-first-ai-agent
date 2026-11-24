import { Children, isValidElement, useEffect, useRef, useState, type ReactNode } from "react";
import Markdown from "react-markdown";
import type { Components } from "react-markdown";
import { offlineMarkdown } from "./markdown";

function textContent(content: ReactNode): string {
  if (typeof content === "string" || typeof content === "number") return String(content);
  if (isValidElement<{ children?: ReactNode }>(content)) return textContent(content.props.children);
  if (Array.isArray(content)) return Children.toArray(content).map(textContent).join("");
  return "";
}

async function copyLocalText(text: string): Promise<void> {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return;
    }
  } catch {
    // Some WebViews expose the clipboard API but reject custom-protocol origins.
  }
  const previous = document.activeElement;
  const field = document.createElement("textarea");
  field.className = "clipboard-buffer";
  field.value = text;
  field.readOnly = true;
  document.body.append(field);

  try {
    field.select();
    if (!document.execCommand("copy")) {
      throw new Error("Clipboard access is unavailable.");
    }
  } finally {
    field.remove();
    if (previous instanceof HTMLElement) previous.focus();
  }
}

export function CopyButton({ text, label }: { text: string; label: string }) {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy() {
    window.clearTimeout(timer.current);
    try {
      await copyLocalText(text);
      setStatus("copied");
    } catch {
      setStatus("failed");
    }
    timer.current = window.setTimeout(() => setStatus("idle"), 2200);
  }

  return (
    <button className="quiet small" type="button" onClick={() => void copy()} aria-label={label}>
      <span aria-live="polite">
        {status === "copied" ? "Copied" : status === "failed" ? "Copy unavailable" : label}
      </span>
    </button>
  );
}

const components: Components = {
  ...offlineMarkdown.components,
  pre: ({ children }) => {
    const code = textContent(children).replace(/\n$/, "");
    return (
      <div className="code-block">
        <div className="code-actions"><CopyButton text={code} label="Copy code" /></div>
        <pre>{children}</pre>
      </div>
    );
  },
};

export default function MarkdownMessage({ text }: { text: string }) {
  return <div className="markdown"><Markdown {...offlineMarkdown} components={components}>{text}</Markdown></div>;
}
