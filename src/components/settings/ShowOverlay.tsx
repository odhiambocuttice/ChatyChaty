import React from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SegmentedControl } from "../ui/SegmentedControl";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import type { OverlayPosition, OverlayStyle } from "@/bindings";

interface ShowOverlayProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ShowOverlay: React.FC<ShowOverlayProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const styleOptions = [
      {
        value: "none",
        label: t("settings.advanced.overlay.style.options.none"),
      },
      {
        value: "minimal",
        label: t("settings.advanced.overlay.style.options.minimal"),
      },
      {
        value: "live",
        label: t("settings.advanced.overlay.style.options.live"),
      },
    ];

    const positionOptions = [
      {
        value: "bottom",
        label: t("settings.advanced.overlay.position.options.bottom"),
      },
      {
        value: "top",
        label: t("settings.advanced.overlay.position.options.top"),
      },
      {
        value: "left",
        label: t("settings.advanced.overlay.position.options.left"),
      },
      {
        value: "right",
        label: t("settings.advanced.overlay.position.options.right"),
      },
    ];

    const selectedStyle = (getSetting("overlay_style") ||
      "live") as OverlayStyle;
    const rawPos = getSetting("overlay_position");
    const selectedPosition: OverlayPosition =
      rawPos === "top" || rawPos === "left" || rawPos === "right"
        ? rawPos
        : "bottom";

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.overlay.style.label")}
          description={t("settings.advanced.overlay.style.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={styleOptions}
            selectedValue={selectedStyle}
            onSelect={(value) =>
              updateSetting("overlay_style", value as OverlayStyle)
            }
            disabled={isUpdating("overlay_style")}
          />
        </SettingContainer>

        {selectedStyle !== "none" && (
          <SettingContainer
            title={t("settings.advanced.overlay.position.label")}
            description={t("settings.advanced.overlay.position.description")}
            descriptionMode={descriptionMode}
            grouped={grouped}
          >
            <SegmentedControl
              ariaLabel={t("settings.advanced.overlay.position.title")}
              options={positionOptions}
              value={selectedPosition}
              onChange={(value) =>
                updateSetting("overlay_position", value as OverlayPosition)
              }
              disabled={isUpdating("overlay_position")}
            />
          </SettingContainer>
        )}
      </>
    );
  },
);
