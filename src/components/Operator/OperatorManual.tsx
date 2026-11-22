import { useLayoutEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { openExternalUrl } from "../../lib/commands";
import { OperatorDialog } from "./OperatorDialog";
import { manualHome as home } from "./manualTopic";
import "./manual.css";

const files = import.meta.glob<string>("../../../docs/{operator-guide,operator-rehearsal}/**/README.md", {
  eager: true, query: "?raw", import: "default",
});
const chapters = Object.entries(files).map(([path, content]) => ({
  path: path.slice(path.indexOf("docs/") + 5),
  title: content.match(/^# (.+)$/m)?.[1] ?? path,
  content,
  searchText: content.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/[*#|`]/g, "").replace(/\s+/g, " ").trim(),
}));

type ManualLocation = { path: string; heading?: string };

function resolveLocation(link: string, from: string): ManualLocation {
  const url = new URL(link, "https://manual.invalid/" + from);
  return { path: url.pathname.slice(1), heading: url.hash ? decodeURIComponent(url.hash.slice(1)) : undefined };
}

const contents = chapters.find(chapter => chapter.path === home)?.content ?? "";
const order = [home, ...Array.from(contents.matchAll(/^- \[[^\]]+\]\(([^)]+)\)/gm), match => resolveLocation(match[1], home).path)];
const rank = (path: string) => order.includes(path) ? order.indexOf(path) : order.length;
chapters.sort((a, b) => rank(a.path) - rank(b.path) || a.title.localeCompare(b.title));

export default function OperatorManual({ onClose, initialChapter = home }: { onClose: () => void; initialChapter?: string }) {
  const [history, setHistory] = useState<ManualLocation[]>(() => initialChapter === home ? [{ path: home }] : [{ path: home }, { path: initialChapter }]);
  const [query, setQuery] = useState("");
  const [error, setError] = useState("");
  const { path: selected, heading } = history[history.length - 1];
  const article = useRef<HTMLElement>(null);
  const active = chapters.find(chapter => chapter.path === selected);
  const terms = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  const matches = chapters.filter(chapter => terms.every(term => chapter.searchText.toLowerCase().includes(term)));

  useLayoutEffect(() => {
    const pane = article.current;
    if (!pane) return;
    pane.scrollTop = 0;
    if (!heading) return;
    const target = Array.from(pane.querySelectorAll<HTMLElement>("h1,h2,h3,h4,h5,h6")).find(element => {
      const slug = element.textContent?.trim().toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, "").replace(/\s+/g, "-");
      return slug === heading;
    });
    if (target) {
      target.tabIndex = -1;
      target.focus({ preventScroll: true });
      pane.scrollTop = target.getBoundingClientRect().top - pane.getBoundingClientRect().top;
    }
  }, [selected, heading]);

  function navigate(location: ManualLocation) {
    if (!chapters.some(chapter => chapter.path === location.path)) {
      setError("This reference is in the source distribution.");
      return;
    }
    setError("");
    if (location.path !== selected || location.heading !== heading) setHistory(previous => [...previous, location]);
  }
  return <OperatorDialog title="Operator manual" onClose={onClose}>
    <div className="operator-library">
      <nav aria-label="Manual chapters">
        <label>Find a topic<input type="search" value={query} onChange={event => setQuery(event.target.value)} /></label>
        {matches.length === 0 && <p>No matching chapters.</p>}
        {matches.map(chapter => {
          const at = terms.length ? chapter.searchText.toLowerCase().indexOf(terms[0]) : 0;
          return <button key={chapter.path} aria-current={selected === chapter.path ? "page" : undefined}
            onClick={() => navigate({ path: chapter.path })}>
            {chapter.title}
            {terms.length > 0 && <small>{at > 28 ? "…" : ""}{chapter.searchText.slice(Math.max(0, at - 28), at + 66)}…</small>}
          </button>;
        })}
      </nav>
      <article key={selected} ref={article} tabIndex={0} aria-label={active?.title ?? "Chapter"}>
        {history.length > 1 && <button onClick={() => { setHistory(previous => previous.slice(0, -1)); setError(""); }}>← Back in manual</button>}
        {error && <p role="alert">{error}</p>}
        <Markdown remarkPlugins={[remarkGfm]} components={{ a: ({ href, children }) => <a href={href}
          onClick={event => {
            event.preventDefault();
            if (!href) return;
            if (/^https?:\/\//i.test(href)) {
              void openExternalUrl(href).catch(e => setError(String(e)));
              return;
            }
            navigate(resolveLocation(href, selected));
          }}>{children}</a> }}>{active?.content ?? "This chapter is unavailable."}</Markdown>
      </article>
    </div>
  </OperatorDialog>;
}
