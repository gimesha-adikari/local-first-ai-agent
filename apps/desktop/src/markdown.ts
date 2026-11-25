import { createElement } from "react";
import type { Components, Options } from "react-markdown";
import remarkGfm from "remark-gfm";

const components: Components = {
  a: ({ children }) => createElement("span", { className: "markdown-link" }, children),
  // Wide GFM tables scroll inside their own region instead of widening the thread.
  table: ({ children }) => createElement(
    "div", { className: "markdown-table", tabIndex: 0, role: "region", "aria-label": "Table" },
    createElement("table", null, children),
  ),
};

export const offlineMarkdown: Options = {
  remarkPlugins: [remarkGfm],
  skipHtml: true,
  disallowedElements: ["img"],
  urlTransform: () => "",
  components,
};
