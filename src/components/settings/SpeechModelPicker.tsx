import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type SpeechModelOption } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";

/**
 * The speech models to pick from. Picking one downloads it if needed and
 * loads it; ModelStatus shows the progress.
 */
export const SpeechModelPicker: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [models, setModels] = useState<SpeechModelOption[]>([]);

  useEffect(() => {
    commands.getSpeechModels().then(setModels);
  }, []);

  const selected = getSetting("speech_model") ?? "parakeet";
  const busy = isUpdating("speech_model");

  return (
    <div role="radiogroup" aria-label={t("settings.groups.speechModel")}>
      {models.map((model) => {
        const on = model.id === selected;
        return (
          <button
            key={model.id}
            type="button"
            role="radio"
            aria-checked={on}
            disabled={busy}
            onClick={() => updateSetting("speech_model", model.id)}
            className="w-full py-4 flex items-center gap-4 text-start cursor-pointer disabled:cursor-not-allowed [&+&]:border-t [&+&]:border-line focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary rounded-sm"
          >
            <span
              className={`size-5 shrink-0 rounded-full border-2 grid place-items-center ${
                on ? "border-background-ui" : "border-line-strong"
              }`}
            >
              {on && (
                <span className="size-2.5 rounded-full bg-background-ui" />
              )}
            </span>
            <span>
              <b className="block text-row font-semibold tracking-[-0.01em]">
                {model.name}
              </b>
              <span className="mt-0.5 block text-sm text-ink-3">
                {t(
                  model.streams
                    ? "settings.speechModel.liveDetails"
                    : "settings.speechModel.afterStopDetails",
                  { size: model.size_mb },
                )}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
};
