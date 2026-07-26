import { useTranslation } from "react-i18next";

import type { TlvDto } from "../ipc";

interface Props {
  /** The parameters currently declared. */
  readonly tlvs: readonly TlvDto[];
  /** Called with the new list. */
  readonly onChange: (tlvs: TlvDto[]) => void;
}

/**
 * The custom optional-parameter editor (fiche §2).
 *
 * A tag and a raw hexadecimal value, and nothing interpreted: the whole point
 * of a custom TLV is that the application does not know what it means. The tag
 * is typed in hexadecimal because that is how every message-centre
 * documentation writes it — `0x1403`, not `5123`.
 *
 * **Nothing is validated here.** A tag out of range or a value with an odd
 * number of digits is refused by the backend with `MESSAGE_INVALID_TLV`, which
 * the notification translates. Validating in the WebView too would be a second
 * rule to keep in step with the first, and the backend would still have to
 * apply its own (CLAUDE.md §3).
 */
export function TlvEditor({ tlvs, onChange }: Props) {
  const { t } = useTranslation();

  const replace = (index: number, patch: Partial<TlvDto>) => {
    onChange(tlvs.map((tlv, position) => (position === index ? { ...tlv, ...patch } : tlv)));
  };

  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="text-xs font-medium">{t("send.tlv.legend")}</legend>

      {tlvs.length === 0 ? <p className="text-xs opacity-60">{t("send.tlv.empty")}</p> : null}

      {tlvs.map((tlv, index) => (
        // The index is the key, and here that is a decision rather than
        // laziness: a TLV has no identity of its own, the list is a handful of
        // rows, and a row is only ever appended or removed whole — so the
        // reordering a positional key mishandles cannot happen.
        <div key={index} className="flex flex-wrap items-end gap-2">
          <div className="flex flex-col gap-1">
            <label htmlFor={`tlv-tag-${index}`} className="text-xs opacity-70">
              {t("send.tlv.tag")}
            </label>
            <input
              id={`tlv-tag-${index}`}
              value={tlv.tag.toString(16).toUpperCase().padStart(4, "0")}
              onChange={(event) => {
                const parsed = Number.parseInt(event.target.value, 16);

                replace(index, { tag: Number.isNaN(parsed) ? 0 : parsed & 0xffff });
              }}
              className="w-28 rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 font-mono text-sm"
            />
          </div>

          <div className="flex min-w-48 flex-1 flex-col gap-1">
            <label htmlFor={`tlv-value-${index}`} className="text-xs opacity-70">
              {t("send.tlv.value")}
            </label>
            <input
              id={`tlv-value-${index}`}
              value={tlv.valueHex}
              placeholder="DEADBEEF"
              onChange={(event) => replace(index, { valueHex: event.target.value })}
              className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 font-mono text-sm"
            />
          </div>

          <button
            type="button"
            onClick={() => onChange(tlvs.filter((_, position) => position !== index))}
            className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)]"
          >
            {t("send.tlv.remove")}
          </button>
        </div>
      ))}

      <div>
        <button
          type="button"
          onClick={() => onChange([...tlvs, { tag: 0x1403, valueHex: "" }])}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)]"
        >
          {t("send.tlv.add")}
        </button>
      </div>
    </fieldset>
  );
}
