import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface PasteWithClipboardProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const PasteWithClipboard: React.FC<PasteWithClipboardProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("paste_with_clipboard") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("paste_with_clipboard", value)}
        isUpdating={isUpdating("paste_with_clipboard")}
        label={t("settings.advanced.pasteWithClipboard.label")}
        description={t("settings.advanced.pasteWithClipboard.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        tooltipPosition="bottom"
      />
    );
  },
);
