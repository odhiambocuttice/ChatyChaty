import React, { useId, useState } from "react";
import { ChevronRight, type LucideIcon } from "lucide-react";

interface SettingsGroupProps {
  title?: string;
  description?: string;
  /** With an icon, the group is a row that folds open, like a macOS
   * System Settings row. */
  icon?: LucideIcon | React.ComponentType<{ className?: string }>;
  defaultOpen?: boolean;
  children: React.ReactNode;
}

/** An orange rounded tile holding a white icon. */
export const IconTile: React.FC<{
  icon: LucideIcon | React.ComponentType<{ className?: string }>;
  size?: "sm" | "md";
}> = ({ icon: Icon, size = "md" }) => (
  <span
    className={`inline-flex items-center justify-center shrink-0 bg-logo-primary text-white ${
      size === "sm" ? "size-6 rounded-md" : "size-7 rounded-lg"
    }`}
  >
    <Icon className={size === "sm" ? "size-3.5" : "size-4"} />
  </span>
);

export const SettingsGroup: React.FC<SettingsGroupProps> = ({
  title,
  description,
  icon,
  defaultOpen = false,
  children,
}) => {
  const [open, setOpen] = useState(defaultOpen);
  const panelId = useId();

  if (icon && title) {
    return (
      <div className="bg-panel rounded-panel overflow-visible">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={panelId}
          onClick={() => setOpen((o) => !o)}
          className="w-full flex items-center gap-3 px-4 min-h-14 py-2.5 text-start cursor-pointer rounded-panel focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
        >
          <IconTile icon={icon} />
          <span className="flex-1 min-w-0">
            <span className="block text-row font-medium tracking-[-0.01em]">
              {title}
            </span>
            {description && (
              <span className="block text-small text-ink-3 mt-0.5">
                {description}
              </span>
            )}
          </span>
          <ChevronRight
            className={`size-4 text-ink-3 shrink-0 transition-transform rtl:-scale-x-100 ${
              open ? "rotate-90" : ""
            }`}
          />
        </button>
        {open && (
          <div id={panelId} className="px-5 border-t border-line">
            <div className="divide-y divide-line">{children}</div>
          </div>
        )}
      </div>
    );
  }

  return (
    <div>
      {title && (
        <div className="mb-2.5 ms-1">
          <h2 className="text-small font-semibold text-ink-3">{title}</h2>
          {description && (
            <p className="text-small text-ink-3 mt-1">{description}</p>
          )}
        </div>
      )}
      <div className="bg-panel rounded-panel px-5 overflow-visible">
        <div className="divide-y divide-line">{children}</div>
      </div>
    </div>
  );
};
