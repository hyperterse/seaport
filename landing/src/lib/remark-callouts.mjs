import { visit } from "unist-util-visit";

const LABELS = {
  note: "Note",
  tip: "Tip",
  info: "Note",
  warning: "Warning",
  warn: "Warning",
  danger: "Caution",
  caution: "Caution",
};

const ALIASES = {
  info: "note",
  warn: "warning",
  caution: "danger",
};

/**
 * Turns `:::note` / `:::tip` / `:::warning` / `:::danger` container directives
 * into styled <aside class="callout callout-<kind>"> blocks. Markdown inside
 * the directive is parsed normally.
 */
export default function remarkCallouts() {
  return (tree) => {
    visit(tree, (node) => {
      if (node.type !== "containerDirective") return;

      const name = node.name?.toLowerCase();
      if (!name || !(name in LABELS)) return;

      const kind = ALIASES[name] ?? name;
      const label = node.attributes?.title || LABELS[name];

      const data = node.data || (node.data = {});
      data.hName = "aside";
      data.hProperties = {
        className: ["callout", `callout-${kind}`],
        "data-label": label,
      };
    });
  };
}
