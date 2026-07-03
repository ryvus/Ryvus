import { useEffect, useState } from "react";
import ReactMarkdown from "react-markdown";
import { loadDocPage } from "../artifacts/load";
import type { Artifacts, DocsRegistryPage } from "../artifacts/types";
import { CodeBlock, EmptyState, Page, Panel, cn } from "../components/ui";

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
    <Page eyebrow="Project" title="Docs">
      <div className="grid gap-4 lg:grid-cols-[260px_minmax(0,1fr)]">
      <Panel className="grid content-start gap-2 p-3">
        {pages.map((page) => (
          <button
            key={page.id}
            type="button"
            className={cn(
              "rounded-lg border border-transparent px-3 py-2 text-left text-sm font-medium text-slate-400 transition hover:border-blue-400/20 hover:bg-white/[0.04] hover:text-white",
              selected?.id === page.id && "border-blue-400/25 bg-blue-500/10 text-white",
            )}
            onClick={() => setSelected(page)}
          >
            {page.title}
          </button>
        ))}
      </Panel>
      <Panel className="min-w-0 p-5">
        {error && <p className="rounded-lg border border-red-400/20 bg-red-500/10 p-3 text-sm text-red-200">{error}</p>}
        {!selected ? (
          <EmptyState title="No docs" message="No docs pages in this artifact snapshot." />
        ) : selected.content_type === "Markdown" ? (
          <article className="portal-markdown">
            <ReactMarkdown>{content}</ReactMarkdown>
          </article>
        ) : (
          <CodeBlock>{content}</CodeBlock>
        )}
      </Panel>
      </div>
    </Page>
  );
}
