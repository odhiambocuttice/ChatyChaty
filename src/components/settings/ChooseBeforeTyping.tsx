import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface ChooseBeforeTypingProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ChooseBeforeTyping: React.FC<ChooseBeforeTypingProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("choose_before_typing") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("choose_before_typing", value)}
        isUpdating={isUpdating("choose_before_typing")}
        label={t("settings.advanced.chooseBeforeTyping.label")}
        description={t("settings.advanced.chooseBeforeTyping.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        tooltipPosition="bottom"
      />
    );
  },
);
