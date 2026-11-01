import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { Button } from "./ui/Button";

/**
 * The log file, so it can be copied into a bug report. Rotation keeps the
 * file at 500 KB, so it is shown whole.
 */
export const LogViewer: React.FC = () => {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const box = useRef<HTMLTextAreaElement>(null);

  const refresh = useCallback(async () => {
    const result = await commands.getLogText();
    if (result.status === "ok") {
      setText(result.data);
      setError(null);
    } else {
      setError(result.error);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Scroll to the newest lines whenever the text changes.
  useEffect(() => {
    const el = box.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Older webviews: select the text and let the browser copy it.
      box.current?.select();
      document.execCommand("copy");
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="py-4 space-y-3">
      <p className="text-sm text-ink-3">{t("settings.logs.hint")}</p>
      <textarea
        ref={box}
        readOnly
        value={text}
        placeholder={t("settings.logs.empty")}
        spellCheck={false}
        className="w-full h-72 p-3 text-xs font-mono leading-snug bg-background border border-line rounded-control resize-y select-text cursor-text focus:outline-none focus:border-logo-primary"
      />
      {error && <p className="text-xs text-error">{error}</p>}
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="primary" size="md" onClick={copy} disabled={!text}>
          {copied ? t("settings.logs.copied") : t("settings.logs.copy")}
        </Button>
        <Button variant="secondary" size="md" onClick={refresh}>
          {t("settings.logs.refresh")}
        </Button>
        <Button variant="ghost" size="md" onClick={() => commands.openLogDir()}>
          {t("settings.logs.openFolder")}
        </Button>
      </div>
    </div>
  );
};
