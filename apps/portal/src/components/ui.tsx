import { useState, type ButtonHTMLAttributes, type PropsWithChildren, type ReactNode } from "react";

export function cn(...values: Array<string | false | null | undefined>) {
  return values.filter(Boolean).join(" ");
}

export function Page({
  eyebrow,
  title,
  children,
  actions,
}: PropsWithChildren<{ eyebrow?: string; title: string; actions?: ReactNode }>) {
  return (
    <div className="mx-auto grid w-full max-w-[1720px] gap-5">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
        <div className="grid gap-1">
          {eyebrow && <span className="text-[11px] font-semibold uppercase text-blue-400">{eyebrow}</span>}
          <h1 className="text-2xl font-semibold tracking-tight text-white sm:text-3xl">{title}</h1>
        </div>
        {actions}
      </div>
      {children}
    </div>
  );
}

export function Panel({
  children,
  className,
}: PropsWithChildren<{ className?: string }>) {
  return (
    <section
      className={cn(
        "rounded-xl border border-white/10 bg-slate-950/72 shadow-[0_18px_60px_rgba(2,6,23,0.34)] backdrop-blur",
        className,
      )}
    >
      {children}
    </section>
  );
}

export function Badge({
  children,
  tone = "blue",
}: PropsWithChildren<{ tone?: "blue" | "violet" | "cyan" | "green" | "red" | "amber" | "slate" }>) {
  const tones = {
    blue: "border-blue-400/25 bg-blue-500/10 text-blue-200",
    violet: "border-violet-400/25 bg-violet-500/10 text-violet-200",
    cyan: "border-cyan-400/25 bg-cyan-500/10 text-cyan-100",
    green: "border-emerald-400/25 bg-emerald-500/10 text-emerald-200",
    red: "border-red-400/25 bg-red-500/10 text-red-200",
    amber: "border-amber-300/30 bg-amber-400/10 text-amber-100",
    slate: "border-white/10 bg-white/5 text-slate-300",
  };

  return (
    <span className={cn("inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium", tones[tone])}>
      {children}
    </span>
  );
}

export function Button({
  className,
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      className={cn(
        "inline-flex min-h-9 items-center justify-center rounded-lg bg-blue-500 px-3 text-sm font-semibold text-white shadow-sm shadow-blue-950/30 transition hover:bg-blue-400 disabled:cursor-not-allowed disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-300",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

export function EmptyState({ title, message }: { title: string; message: string }) {
  return (
    <Panel className="grid min-h-52 place-items-center p-8 text-center">
      <div className="grid max-w-md justify-items-center gap-3">
        <span className="h-2.5 w-2.5 rounded-full bg-gradient-to-br from-blue-400 to-violet-500 shadow-[0_0_0_6px_rgba(37,99,255,0.14)]" />
        <h2 className="text-lg font-semibold text-white">{title}</h2>
        <p className="text-sm leading-6 text-slate-400">{message}</p>
      </div>
    </Panel>
  );
}

export function CodeBlock({ children, className }: PropsWithChildren<{ className?: string }>) {
  const [copied, setCopied] = useState(false);
  const text = typeof children === "string" ? children : String(children ?? "");

  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="group relative min-w-0">
      <button
        type="button"
        onClick={copy}
        className="absolute right-2 top-2 z-10 rounded-md border border-white/10 bg-slate-950/90 px-2 py-1 text-[11px] font-medium text-slate-300 opacity-0 shadow-lg transition hover:bg-slate-900 hover:text-white group-hover:opacity-100 focus:opacity-100"
      >
        {copied ? "Copied" : "Copy"}
      </button>
      <pre className={cn("max-w-full overflow-auto rounded-lg border border-white/10 bg-black/35 p-3 pr-16 text-xs leading-5 text-slate-200", className)}>
        {children}
      </pre>
    </div>
  );
}
