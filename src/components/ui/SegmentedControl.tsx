import React from "react";

interface SegmentedControlProps {
  options: { value: string; label: string }[];
  value: string;
  onChange: (value: string) => void;
  ariaLabel: string;
  disabled?: boolean;
}

/** A short list of choices shown side by side; one is picked. */
export const SegmentedControl: React.FC<SegmentedControlProps> = ({
  options,
  value,
  onChange,
  ariaLabel,
  disabled = false,
}) => (
  <div
    role="radiogroup"
    aria-label={ariaLabel}
    className="flex gap-0.5 p-0.5 rounded-[9px] bg-line"
  >
    {options.map((option) => {
      const on = option.value === value;
      return (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={on}
          disabled={disabled}
          onClick={() => onChange(option.value)}
          className={`min-h-[30px] px-4 rounded-[7px] text-sm font-medium cursor-pointer disabled:cursor-not-allowed disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary ${
            on ? "bg-background shadow-sm" : "hover:bg-wash"
          }`}
        >
          {option.label}
        </button>
      );
    })}
  </div>
);
