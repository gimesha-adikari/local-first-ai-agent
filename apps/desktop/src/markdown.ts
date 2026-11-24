import { createElement } from "react";
import type { Components, Options } from "react-markdown";

const components: Components = {
  a: ({ children }) => createElement("span", { className: "markdown-link" }, children),
};

export const offlineMarkdown: Options = {
  skipHtml: true,
  disallowedElements: ["img"],
  urlTransform: () => "",
  components,
};
