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
    <div className="mx-auto grid w-full max-w-[1720px] gap-4">
      <div className="flex flex-col gap-3 border-b border-white/10 pb-4 sm:flex-row sm:items-end sm:justify-between">
        <div className="grid gap-1">
          {eyebrow && <span className="font-mono text-[11px] font-bold uppercase text-slate-500">{eyebrow}</span>}
          <h1 className="text-xl font-semibold tracking-tight text-white sm:text-2xl">{title}</h1>
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
        "rounded-lg border border-white/10 bg-[#111214]/95 shadow-[0_1px_0_rgba(255,255,255,0.03)]",
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
    blue: "border-blue-400/20 bg-blue-500/10 text-blue-200",
    violet: "border-violet-300/24 bg-violet-500/12 text-violet-100",
    cyan: "border-cyan-300/20 bg-cyan-400/10 text-cyan-100",
    green: "border-emerald-400/20 bg-emerald-500/12 text-emerald-300",
    red: "border-red-400/22 bg-red-500/12 text-red-300",
    amber: "border-amber-300/24 bg-amber-400/10 text-amber-200",
    slate: "border-white/10 bg-white/[0.04] text-slate-400",
  };

  return (
    <span className={cn("inline-flex items-center rounded-md border px-2 py-0.5 font-mono text-[11px] font-bold uppercase", tones[tone])}>
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
        "inline-flex min-h-9 items-center justify-center rounded-md border border-white/15 bg-slate-100 px-3 text-sm font-semibold text-slate-950 transition-[transform,background-color,border-color,opacity] hover:bg-white active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300",
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
    <Panel className="grid min-h-52 place-items-center p-6 text-center sm:p-8">
      <div className="grid min-w-0 w-full max-w-md justify-items-center gap-3">
        <span className="h-2 w-2 rounded-sm bg-violet-400 shadow-[0_0_0_6px_rgba(111,61,255,0.12)]" />
        <h2 className="text-lg font-semibold text-white">{title}</h2>
        <p className="mx-auto w-full max-w-xs whitespace-normal text-sm leading-6 text-slate-400 sm:max-w-md">{message}</p>
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
        className="absolute right-2 top-2 z-10 rounded-md border border-white/10 bg-[#111214] px-2 py-1 font-mono text-[11px] font-semibold text-slate-400 opacity-0 transition hover:bg-white/[0.06] hover:text-white group-hover:opacity-100 focus:opacity-100"
      >
        {copied ? "Copied" : "Copy"}
      </button>
      <pre className={cn("max-w-full overflow-auto rounded-md border border-white/10 bg-[#050506] p-3 pr-16 text-xs leading-5 text-slate-200", className)}>
        {children}
      </pre>
    </div>
  );
}
