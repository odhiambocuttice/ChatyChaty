import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type PhoneStatus } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { Button } from "../ui/Button";

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

  return (
    <div
      className={`flex max-w-md flex-col items-start gap-2 text-sm ${grouped ? "pb-4" : "mt-2 px-4"}`}
    >
      {status?.qr_svg && (
        <>
          <p>{t("settings.sound.microphone.phoneScan")}</p>
          <div
            className="h-[200px] w-[200px] rounded-lg bg-white p-2 [&>svg]:h-full [&>svg]:w-full"
            dangerouslySetInnerHTML={{ __html: status.qr_svg }}
          />
        </>
      )}
      {status?.url && <p className="select-text break-all">{status.url}</p>}
      {status?.running && (
        <p className="font-medium">
          {status.connected
            ? t("settings.sound.microphone.phoneConnected")
            : t("settings.sound.microphone.phoneWaiting")}
        </p>
      )}
      {error && <p className="text-red-500 select-text">{error}</p>}
      <p className="text-text/70">
        {t("settings.sound.microphone.phoneCertNote")}
      </p>
      <p className="text-text/70">
        {t("settings.sound.microphone.phoneFirewall")}
      </p>
      <Button variant="secondary" size="sm" onClick={() => void newCode()}>
        {t("settings.sound.microphone.phoneNewCode")}
      </Button>
    </div>
  );
};
