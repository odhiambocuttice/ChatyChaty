import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type PhoneStatus } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { platform } from "@tauri-apps/plugin-os";
import { Button } from "../ui/Button";

const isWindows = (() => {
  try {
    return platform() === "windows";
  } catch {
    return false;
  }
})();

/** QR code and status for the "Phone (Wi-Fi)" microphone. */
export const PhoneMicrophonePanel: React.FC<{ grouped?: boolean }> = ({
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const active = getSetting("selected_microphone") === "phone-wifi";
  const [status, setStatus] = useState<PhoneStatus | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  const load = useCallback(async () => {
    const result = await commands.getPhoneMicrophoneStatus();
    if (result.status === "ok") {
      setStatus(result.data);
      setFailure(null);
    } else {
      setFailure(result.error);
    }
  }, []);

  useEffect(() => {
    if (!active) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void load();
    listen("phone-microphone-changed", () => void load()).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [active, load]);

  const newCode = async () => {
    const result = await commands.newPhoneMicrophoneCode();
    if (result.status === "ok") {
      setStatus(result.data);
      setFailure(null);
    } else {
      setFailure(result.error);
    }
  };

  if (!active) return null;

  const error = failure ?? status?.error ?? null;

  const steps = [
    t("settings.sound.microphone.phoneSteps.wifi"),
    t("settings.sound.microphone.phoneSteps.scan"),
    t("settings.sound.microphone.phoneSteps.advanced"),
    t("settings.sound.microphone.phoneSteps.record"),
  ];

  return (
    <div className={`text-sm ${grouped ? "py-4" : "mt-2 px-4"}`}>
      <div className="flex items-center justify-between gap-4">
        <h3 className="text-row font-semibold tracking-[-0.01em]">
          {t("settings.sound.microphone.phoneSteps.title")}
        </h3>
        {status?.running && (
          <span className="flex items-center gap-2">
            <span
              className={`size-2.5 rounded-full ${
                status.connected ? "bg-green-500" : "bg-amber-500 animate-pulse"
              }`}
            />
            <span className="font-medium">
              {status.connected
                ? t("settings.sound.microphone.phoneConnected")
                : t("settings.sound.microphone.phoneWaiting")}
            </span>
          </span>
        )}
      </div>
      <ol className="mt-3 space-y-3">
        {steps.map((text, index) => (
          <li key={index} className="flex gap-3">
            <span className="size-6 shrink-0 inline-flex items-center justify-center rounded-full bg-logo-primary text-white text-xs font-semibold">
              {index + 1}
            </span>
            <div className="flex-1 min-w-0 pt-0.5">
              {text}
              {index === 1 && status?.qr_svg && (
                <div className="mt-2 flex items-end gap-3">
                  <div
                    className="h-[180px] w-[180px] rounded-lg bg-white p-2 [&>svg]:h-full [&>svg]:w-full"
                    dangerouslySetInnerHTML={{ __html: status.qr_svg }}
                  />
                  <Button
                    variant="primary"
                    size="md"
                    className="min-h-9"
                    onClick={() => void newCode()}
                  >
                    {t("settings.sound.microphone.phoneNewCode")}
                  </Button>
                </div>
              )}
            </div>
          </li>
        ))}
      </ol>
      {error && <p className="mt-3 text-error select-text">{error}</p>}
      {isWindows && (
        <p className="mt-3 text-small text-ink-3">
          {t("settings.sound.microphone.phoneFirewall")}
        </p>
      )}
    </div>
  );
};
