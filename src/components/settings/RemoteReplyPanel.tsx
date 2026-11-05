import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type TelegramStatus } from "@/bindings";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

export const RemoteReplyPanel: React.FC = () => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<TelegramStatus | null>(null);
  const [tokenInput, setTokenInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const refresh = () => {
    commands.getTelegramStatus().then((res) => {
      if (res.status === "ok") {
        setStatus(res.data);
      }
    });
  };

  useEffect(() => {
    refresh();
    const unlisten = listen("telegram-paired", () => {
      refresh();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const saveToken = async (token: string) => {
    setBusy(true);
    setError(null);
    const res = await commands.setTelegramBotToken(token);
    setBusy(false);
    if (res.status === "error") {
      setError(t("settings.remoteReply.saveFailed", { error: res.error }));
      return;
    }
    setTokenInput("");
    setStatus(res.data);
  };

  const unlink = async () => {
    setBusy(true);
    setError(null);
    const res = await commands.unlinkTelegram();
    setBusy(false);
    if (res.status === "ok") {
      setStatus(res.data);
    }
  };

  const copyCode = (code: string) => {
    navigator.clipboard.writeText(`/pair ${code}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <>
      <SettingContainer
        title={t("settings.remoteReply.botToken")}
        description={t("settings.remoteReply.description")}
        descriptionMode="tooltip"
        grouped={true}
      >
        <div className="flex flex-col items-end gap-1">
          <div className="flex items-center gap-2">
            <input
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={tokenInput}
              onChange={(e) => setTokenInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && tokenInput.trim())
                  saveToken(tokenInput);
              }}
              placeholder={
                status?.configured
                  ? status.bot_username
                    ? `@${status.bot_username}`
                    : t("settings.replyKeys.saved")
                  : t("settings.remoteReply.botTokenPlaceholder")
              }
              disabled={busy}
              className="min-h-9 px-3 py-[5px] text-sm bg-background border border-line-strong rounded-control w-[240px] focus:outline-none focus:border-logo-primary disabled:opacity-50"
            />
            <Button
              variant="secondary"
              size="sm"
              onClick={() => saveToken(tokenInput)}
              disabled={busy || !tokenInput.trim()}
            >
              {t("settings.remoteReply.saveToken")}
            </Button>
            {status?.configured && (
              <Button
                variant="danger-ghost"
                size="sm"
                onClick={() => saveToken("")}
                disabled={busy}
              >
                {t("settings.remoteReply.removeToken")}
              </Button>
            )}
          </div>
          {error && <p className="text-xs text-error">{error}</p>}
        </div>
      </SettingContainer>

      {status?.configured && (
        <div className="p-4 bg-wash/60 border border-line rounded-lg text-sm space-y-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <span
                className={`size-2.5 rounded-full ${
                  status.paired ? "bg-green-500" : "bg-amber-500 animate-pulse"
                }`}
              />
              <span className="font-medium">
                {status.paired
                  ? t("settings.remoteReply.statusPaired")
                  : t("settings.remoteReply.statusUnpaired")}
              </span>
              {status.bot_username && (
                <span className="text-ink-3">(@{status.bot_username})</span>
              )}
            </div>

            {status.paired && (
              <Button
                variant="ghost"
                size="sm"
                onClick={unlink}
                disabled={busy}
              >
                {t("settings.remoteReply.unlink")}
              </Button>
            )}
          </div>

          {!status.paired && status.pairing_code && (
            <div className="bg-background border border-line p-3 rounded-md space-y-2">
              <p className="text-xs text-ink-3">
                {t("settings.remoteReply.pairInstructions")}
              </p>
              <div className="flex items-center gap-3">
                <code className="text-base font-mono font-bold tracking-widest px-2.5 py-1 bg-wash border border-line rounded">
                  {t("settings.remoteReply.pairCommand", {
                    code: status.pairing_code,
                  })}
                </code>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => copyCode(status.pairing_code!)}
                >
                  {copied
                    ? t("settings.remoteReply.copied")
                    : t("settings.remoteReply.copyCommand")}
                </Button>
              </div>
            </div>
          )}

          <p className="text-xs text-ink-3">
            {t("settings.remoteReply.disclaimer")}
          </p>
        </div>
      )}
    </>
  );
};
