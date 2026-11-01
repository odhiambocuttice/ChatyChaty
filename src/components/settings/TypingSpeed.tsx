import React from "react";
import { useTranslation } from "react-i18next";
import { SegmentedControl } from "../ui/SegmentedControl";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import type { TypingSpeed as TypingSpeedValue } from "@/bindings";

interface TypingSpeedProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const TypingSpeed: React.FC<TypingSpeedProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const options = [
      {
        value: "slow",
        label: t("settings.basics.typingSpeed.options.slow"),
      },
      {
        value: "normal",
        label: t("settings.basics.typingSpeed.options.normal"),
      },
      {
        value: "fast",
        label: t("settings.basics.typingSpeed.options.fast"),
      },
    ];

    return (
      <SettingContainer
        title={t("settings.basics.typingSpeed.title")}
        description={t("settings.basics.typingSpeed.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <SegmentedControl
          ariaLabel={t("settings.basics.typingSpeed.title")}
          options={options}
          value={getSetting("typing_speed") || "normal"}
          onChange={(value) =>
            updateSetting("typing_speed", value as TypingSpeedValue)
          }
          disabled={isUpdating("typing_speed")}
        />
      </SettingContainer>
    );
  },
);
