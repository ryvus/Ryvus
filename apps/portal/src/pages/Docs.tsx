import { useEffect, useState } from "react";
import ReactMarkdown from "react-markdown";
import { loadDocPage } from "../artifacts/load";
import type { Artifacts, DocsRegistryPage } from "../artifacts/types";

export function Docs({ artifacts }: { artifacts: Artifacts }) {
  const pages = artifacts.docsRegistry.pages;
  const [selected, setSelected] = useState<DocsRegistryPage | undefined>(pages[0]);
  const [content, setContent] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    setSelected(pages[0]);
  }, [pages]);

  useEffect(() => {
    if (!selected) {
      return;
    }

    loadDocPage(selected)
      .then((value) => {
        setContent(value);
        setError("");
      })
      .catch((err: unknown) => {
        setContent("");
        setError(err instanceof Error ? err.message : "failed to load doc page");
      });
  }, [selected]);

  return (
    <div className="page docs-layout">
      <div className="docs-nav">
        <h1>Docs</h1>
        {pages.map((page) => (
          <button key={page.id} type="button" onClick={() => setSelected(page)}>
            {page.title}
          </button>
        ))}
      </div>
      <article className="doc-content">
        {error && <p className="error">{error}</p>}
        {!selected ? (
          <p>No docs pages in this artifact snapshot.</p>
        ) : selected.content_type === "Markdown" ? (
          <ReactMarkdown>{content}</ReactMarkdown>
        ) : (
          <pre>{content}</pre>
        )}
      </article>
    </div>
  );
}
