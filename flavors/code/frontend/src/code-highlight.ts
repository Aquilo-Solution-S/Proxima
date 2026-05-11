import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import cpp from "highlight.js/lib/languages/cpp";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import toml from "highlight.js/lib/languages/ini";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const highlightLanguages = {
  bash,
  cpp,
  css,
  diff,
  go,
  java,
  javascript,
  json,
  markdown,
  python,
  rust,
  sql,
  toml,
  typescript,
  xml,
  yaml,
};

for (const [name, language] of Object.entries(highlightLanguages)) {
  hljs.registerLanguage(name, language);
}

const languageAliases: Record<string, string> = {
  c: "cpp",
  cc: "cpp",
  cxx: "cpp",
  h: "cpp",
  hpp: "cpp",
  html: "xml",
  js: "javascript",
  jsx: "javascript",
  md: "markdown",
  py: "python",
  rs: "rust",
  sh: "bash",
  shell: "bash",
  ts: "typescript",
  tsx: "typescript",
  yml: "yaml",
};

const asString = (value: unknown): string | null =>
  typeof value === "string" ? value : null;

const extensionFromPath = (value: string): string | null => {
  const clean = value.split(/[?#]/, 1)[0] ?? value;
  const basename = clean.split("/").pop() ?? clean;
  const index = basename.lastIndexOf(".");
  if (index <= 0 || index === basename.length - 1) return null;
  return basename.slice(index + 1);
};

export const normalizedLanguage = (value: unknown): string | null => {
  const raw = asString(value)?.trim().toLowerCase();
  if (!raw) return null;
  const compact = raw.replace(/^tree-sitter-/, "").replace(/[^a-z0-9+#.-]/g, "");
  const alias = languageAliases[compact] ?? compact;
  return hljs.getLanguage(alias) ? alias : null;
};

export const languageFromPath = (path: string): string | null =>
  normalizedLanguage(extensionFromPath(path));

export const highlightedCode = (text: string, language: unknown) => {
  const lang = normalizedLanguage(language);
  if (lang === null) {
    return { html: hljs.highlightAuto(text).value, language: "auto" };
  }
  return { html: hljs.highlight(text, { language: lang }).value, language: lang };
};
