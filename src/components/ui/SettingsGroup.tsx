import React from "react";

interface SettingsGroupProps {
  title?: string;
  description?: string;
  children: React.ReactNode;
}

export const SettingsGroup: React.FC<SettingsGroupProps> = ({
  title,
  description,
  children,
}) => {
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
      <div className="bg-wash rounded-panel px-5 overflow-visible">
        <div className="divide-y divide-line">{children}</div>
      </div>
    </div>
  );
};
