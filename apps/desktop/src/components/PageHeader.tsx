import type { ReactNode } from "react";

/** Standard view header: title, one-line description, optional actions. */
export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description: string;
  actions?: ReactNode;
}) {
  return (
    <header className="mx-auto mb-7 flex max-w-[1440px] items-start justify-between gap-6 max-[680px]:flex-col max-[680px]:gap-3">
      <div>
        <h1 className="m-0 mb-1 text-title-lg font-semibold text-foreground">{title}</h1>
        <p className="m-0 text-body text-muted-foreground">{description}</p>
      </div>
      {actions && <div className="flex items-center gap-2 max-[680px]:w-full max-[680px]:*:flex-1">{actions}</div>}
    </header>
  );
}
