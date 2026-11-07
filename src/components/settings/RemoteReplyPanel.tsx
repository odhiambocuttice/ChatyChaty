import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { commands, type TelegramStatus } from "@/bindings";
import { Trash2 } from "lucide-react";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

/** Telegram's own bot for making bots. Opens in the Telegram app, or a page that does. */
const BOT_FATHER_URL = "https://t.me/BotFather";

/** A step's number in an orange circle. */
const StepNumber: React.FC<{ n: number }> = ({ n }) => (
  <span className="size-6 shrink-0 inline-flex items-center justify-center rounded-full bg-logo-primary text-white text-xs font-semibold">
    {n}
  </span>
);

/** Stands in for a saved token, which is never read back. */
const SAVED_MASK = "••••••••••••";

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

  // The token field: in the Bot Token row once a token is saved, and inside
  // the step that says to paste it while there is none yet.
  const tokenField = (align: "start" | "end") => (
    <div
      className={`flex flex-col gap-1 ${align === "end" ? "items-end" : "items-start"}`}
    >
      <div className="flex items-center gap-2">
        {/* A saved token shows as dots and can't be edited: remove it
            with the bin to enter a different one. */}
        <input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={status?.configured ? SAVED_MASK : tokenInput}
          onChange={(e) => setTokenInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && tokenInput.trim()) saveToken(tokenInput);
          }}
          placeholder={t("settings.remoteReply.botTokenPlaceholder")}
          disabled={busy || !!status?.configured}
          className="min-h-9 px-3 py-[5px] text-sm bg-background border border-line-strong rounded-control w-[240px] focus:outline-none focus:border-logo-primary disabled:opacity-60 disabled:cursor-not-allowed"
        />
        <div className="w-[72px] flex">
          {status?.configured ? (
            <Button
              variant="danger-ghost"
              size="sm"
              onClick={() => saveToken("")}
              disabled={busy}
              title={t("settings.remoteReply.removeToken")}
              aria-label={t("settings.remoteReply.removeToken")}
              className="min-h-9 min-w-9 inline-flex items-center justify-center"
            >
              <Trash2 className="size-4" />
            </Button>
          ) : (
            <Button
              variant="primary"
              size="md"
              className="min-h-9"
              onClick={() => saveToken(tokenInput)}
              disabled={busy || !tokenInput.trim()}
            >
              {t("settings.remoteReply.saveToken")}
            </Button>
          )}
        </div>
      </div>
      {error && <p className="text-xs text-error">{error}</p>}
    </div>
  );

  return (
    <>
      {status?.configured && (
        <SettingContainer
          title={t("settings.remoteReply.botToken")}
          description={t("settings.remoteReply.description")}
          descriptionMode="tooltip"
          grouped={true}
        >
          {tokenField("end")}
        </SettingContainer>
      )}

      {status?.configured && (
        <div className="flex items-center justify-between gap-4 min-h-12 py-4">
          <h3 className="text-row font-semibold tracking-[-0.01em]">
            {t("settings.remoteReply.status")}
          </h3>
          <div className="flex items-center gap-3">
            <span className="flex items-center gap-2 text-sm">
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
                <span className="text-ink-3">@{status.bot_username}</span>
              )}
            </span>
            {status.paired && (
              <Button
                variant="primary"
                size="md"
                className="min-h-9"
                onClick={unlink}
                disabled={busy}
              >
                {t("settings.remoteReply.unlink")}
              </Button>
            )}
          </div>
        </div>
      )}

      {status?.paired && (
        <p className="py-4 text-sm text-ink-3">
          {t("settings.remoteReply.newPhone")}
        </p>
      )}

      {status && !status.configured && (
        <div className="py-4">
          <h3 className="text-row font-semibold tracking-[-0.01em]">
            {t("settings.remoteReply.setup.title")}
          </h3>
          <p className="mt-1 text-sm text-ink-3">
            {t("settings.remoteReply.setup.intro")}
          </p>
          <ol className="mt-3 space-y-3">
            {[
              t("settings.remoteReply.steps.phone"),
              t("settings.remoteReply.setup.botFather"),
              t("settings.remoteReply.setup.newBot"),
              t("settings.remoteReply.setup.names"),
              t("settings.remoteReply.setup.token"),
              t("settings.remoteReply.setup.paste"),
            ].map((text, index) => (
              <li key={index} className="flex gap-3">
                <StepNumber n={index + 1} />
                <div className="flex-1 min-w-0 pt-0.5 text-sm">
                  {text}
                  {index === 5 && (
                    <div className="mt-2">{tokenField("start")}</div>
                  )}
                  {index === 1 && (
                    <div className="mt-2">
                      <Button
                        variant="primary"
                        size="md"
                        className="min-h-9"
                        onClick={() => openUrl(BOT_FATHER_URL)}
                      >
                        {t("settings.remoteReply.setup.openBotFather")}
                      </Button>
                    </div>
                  )}
                </div>
              </li>
            ))}
          </ol>
          <p className="mt-3 text-small text-ink-3">
            {t("settings.remoteReply.setup.private")}
          </p>
        </div>
      )}

      {status?.configured && !status.paired && status.pairing_code && (
        <div className="py-4">
          <h3 className="text-row font-semibold tracking-[-0.01em]">
            {t("settings.remoteReply.steps.title")}
          </h3>
          <ol className="mt-3 space-y-3">
            {[
              t("settings.remoteReply.steps.phone"),
              t("settings.remoteReply.steps.find", {
                bot: status.bot_username
                  ? `@${status.bot_username}`
                  : t("settings.remoteReply.steps.yourBot"),
              }),
              t("settings.remoteReply.steps.send"),
              t("settings.remoteReply.steps.wait"),
            ].map((text, index) => (
              <li key={index} className="flex gap-3">
                <StepNumber n={index + 1} />
                <div className="flex-1 min-w-0 pt-0.5 text-sm">
                  {text}
                  {index === 2 && (
                    <div className="mt-2 flex items-center gap-3">
                      <code className="h-9 inline-flex items-center px-3 text-sm font-mono font-semibold tracking-wider bg-background border border-line-strong rounded-control">
                        {t("settings.remoteReply.pairCommand", {
                          code: status.pairing_code,
                        })}
                      </code>
                      <Button
                        variant="primary"
                        size="md"
                        className="min-h-9"
                        onClick={() => copyCode(status.pairing_code!)}
                      >
                        {copied
                          ? t("settings.remoteReply.copied")
                          : t("settings.remoteReply.copyCommand")}
                      </Button>
                    </div>
                  )}
                </div>
              </li>
            ))}
          </ol>
        </div>
      )}

      {status?.configured && (
        <p className="py-4 text-small text-ink-3">
          {t("settings.remoteReply.disclaimer")}
        </p>
      )}
    </>
  );
};
