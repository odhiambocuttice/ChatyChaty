import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type ProviderKey } from "@/bindings";
import { Trash2 } from "lucide-react";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

/** Stands in for a saved key, which is never read back. */
const SAVED_MASK = "••••••••••••";

interface ProviderKeyRowProps {
  provider: ProviderKey;
  onChanged: () => void;
}

const ProviderKeyRow: React.FC<ProviderKeyRowProps> = ({
  provider,
  onChanged,
}) => {
  const { t } = useTranslation();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // An empty key removes the saved one.
  const save = async (value: string) => {
    setBusy(true);
    setError(null);
    const result = await commands.setReplyProviderKey(provider.name, value);
    setBusy(false);
    if (result.status === "error") {
      setError(t("settings.replyKeys.saveFailed", { error: result.error }));
      return;
    }
    setKey("");
    onChanged();
  };

  return (
    <SettingContainer
      title={provider.name}
      description={t("settings.replyKeys.description")}
      descriptionMode="tooltip"
      grouped={true}
    >
      <div className="flex flex-col items-end gap-1">
        <div className="flex items-center gap-2">
          {/* A saved key shows as dots and can't be edited: remove it with
              the bin to enter a different one. */}
          <input
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={provider.saved ? SAVED_MASK : key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && key.trim()) save(key);
            }}
            placeholder={t("settings.replyKeys.placeholder")}
            disabled={busy || provider.saved}
            aria-label={
              provider.saved ? t("settings.replyKeys.saved") : undefined
            }
            className="min-h-9 px-3 py-[5px] text-sm bg-background border border-line-strong rounded-control w-[200px] focus:outline-none focus:border-logo-primary disabled:opacity-60 disabled:cursor-not-allowed"
          />
          <div className="w-[72px] flex">
            {provider.saved ? (
              <Button
                variant="danger-ghost"
                size="sm"
                onClick={() => save("")}
                disabled={busy}
                title={t("settings.replyKeys.remove")}
                aria-label={t("settings.replyKeys.remove")}
                className="min-h-9 min-w-9 inline-flex items-center justify-center"
              >
                <Trash2 className="size-4" />
              </Button>
            ) : (
              <Button
                variant="primary"
                size="md"
                className="min-h-9"
                onClick={() => save(key)}
                disabled={busy || !key.trim()}
              >
                {t("settings.replyKeys.save")}
              </Button>
            )}
          </div>
        </div>
        {error && <p className="text-xs text-error">{error}</p>}
      </div>
    </SettingContainer>
  );
};

export const ReplyProviderKeys: React.FC = () => {
  const [providers, setProviders] = useState<ProviderKey[]>([]);

  const refresh = () => {
    commands.getReplyProviderKeys().then(setProviders);
  };

  useEffect(refresh, []);

  return (
    <>
      {providers.map((provider) => (
        <ProviderKeyRow
          key={provider.name}
          provider={provider}
          onChanged={refresh}
        />
      ))}
    </>
  );
};
