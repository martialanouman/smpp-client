import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  MessagePreviewDto,
  MessageSendInput,
  MessageSendResultDto,
  SessionProfileDto,
  SessionStatusDto,
} from "../../ipc";
import "../../i18n";
import i18n from "../../i18n";
import fr from "../../i18n/locales/fr.json";
import { useSend } from "../../store/send";
import { useSessions } from "../../store/sessions";
import { SendView } from "./SendView";

const messageSend = vi.fn();
const messagePreview = vi.fn();
const onMessageUpdate = vi.fn();

vi.mock("../../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc")>()),
  messageSend: (input: MessageSendInput) => messageSend(input) as unknown,
  messagePreview: (input: unknown) => messagePreview(input) as unknown,
  onMessageUpdate: (handler: unknown) => onMessageUpdate(handler) as unknown,
}));

/** A bound profile the form can send on. */
function aProfile(sessionId: string): SessionProfileDto {
  return {
    sessionId,
    name: "Operator A",
    host: "smsc.example.test",
    port: 2775,
    bindType: "transceiver",
    interfaceVersion: "v5.0",
    systemId: "esme01",
    systemType: "",
    windowSize: 50,
    minTps: 0,
    throughputTps: 100,
    enquireLinkS: 30,
    responseTimeoutS: 10,
    reconnectEnabled: true,
    minBackoffS: 1,
    maxBackoffS: 60,
    jitter: true,
    gsm7Packing: "unpacked",
    gsm7Charset: "gsm0338",
    bindCount: 1,
  };
}

function aStatus(sessionId: string, state: string): SessionStatusDto {
  return {
    sessionId,
    state,
    bindType: "transceiver",
    lastError: null,
    giveUp: null,
    inFlight: 0,
  };
}

const PREVIEW: MessagePreviewDto = {
  encoding: "gsm7Bit",
  dataCoding: 0,
  characters: 7,
  unitsUsed: 7,
  unitsRemaining: 153,
  segments: 1,
};

const SESSION = "11111111-1111-4111-8111-111111111111";

function anAcceptedResult(): MessageSendResultDto {
  return {
    clientMessageId: "22222222-2222-4222-8222-222222222222",
    sessionId: SESSION,
    state: "ACCEPTED",
    segments: 1,
    smscMessageId: "MSG-42",
    commandStatus: 0,
    statusSymbol: "ESME_ROK",
    statusLabel: "Succès",
    statusIsVendorSpecific: false,
    retryable: false,
    journalled: true,
    outcomes: [
      {
        sequenceNumber: 1,
        outcome: "answered",
        commandStatus: 0,
        statusSymbol: "ESME_ROK",
        smscMessageId: "MSG-42",
      },
    ],
  };
}

describe("Send › Simple", () => {
  beforeEach(async () => {
    // The real catalogues, not the keys: the counter is the one assertion
    // that has to read rendered *numbers*, and `t` without a catalogue returns
    // the key and no interpolation at all.
    await i18n.changeLanguage("fr");

    messageSend.mockReset();
    messagePreview.mockReset();
    onMessageUpdate.mockReset();

    messagePreview.mockResolvedValue({ ok: true, value: PREVIEW });
    onMessageUpdate.mockResolvedValue(() => {});

    useSessions.setState({
      profiles: [aProfile(SESSION)],
      statuses: { [SESSION]: aStatus(SESSION, "BOUND") },
    });
    useSend.setState({
      sessionId: "",
      preview: null,
      result: null,
      progress: null,
      sending: false,
    });
  });

  afterEach(cleanup);

  /// CA-006-09: the counter is what the backend computed, and nothing here
  /// recounts it. A `text.length` in the component would show 7 for a text of
  /// seven `€`, which is 14 septets.
  it("shows the counter the backend computed rather than one of its own", async () => {
    messagePreview.mockResolvedValue({
      ok: true,
      value: { ...PREVIEW, characters: 7, unitsUsed: 14, segments: 1 },
    });

    render(<SendView />);

    await userEvent.type(screen.getByLabelText(fr.send.text), "€€€€€€€");

    await waitFor(() => {
      expect(screen.getByText(/14/u)).toBeTruthy();
    });
  });

  it("refuses to submit before a session and a recipient are chosen", () => {
    render(<SendView />);

    const submit = screen.getByRole("button", { name: fr.send.submit });

    expect((submit as HTMLButtonElement).disabled).toBe(true);
  });

  /// Fiche §6: an alphanumeric sender forces `source_addr_ton = 5` and is not
  /// accepted everywhere. The operator is told before sending, not after.
  it("warns as soon as the sender stops being a number", async () => {
    render(<SendView />);

    const sender = screen.getByLabelText(fr.send.source);

    await userEvent.type(sender, "+2250102030405");
    expect(screen.queryByText(fr.send.alphanumericWarning)).toBeNull();

    await userEvent.clear(sender);
    await userEvent.type(sender, "ShinobiSMS");
    expect(screen.getByText(fr.send.alphanumericWarning)).toBeTruthy();
  });

  it("sends the form and shows the identifier the message centre assigned", async () => {
    messageSend.mockResolvedValue({ ok: true, value: anAcceptedResult() });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.type(screen.getByLabelText(fr.send.text), "Bonjour");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText("MSG-42")).toBeTruthy();
    });

    const sent = messageSend.mock.calls[0]?.[0] as MessageSendInput;

    expect(sent.sessionId).toBe(SESSION);
    expect(sent.destination).toBe("+2250102030405");
    // Spec §23.3: the defaults the form ships with are the safe ones.
    expect(sent.destTon).toBe("international");
    expect(sent.destNpi).toBe("isdn");
    expect(sent.registeredDelivery).toBe("onAnyOutcome");
    expect(sent.encoding).toBe("automatic");
  });

  /// CA-006-05 and ENF-UTI-02: the message centre's own status is shown, with
  /// its symbol and its label, not replaced by a message of ours.
  it("shows a rejection with the raw status the message centre sent", async () => {
    messageSend.mockResolvedValue({
      ok: true,
      value: {
        ...anAcceptedResult(),
        state: "FAILED",
        smscMessageId: null,
        commandStatus: 0x0b,
        statusSymbol: "ESME_RINVDSTADR",
        statusLabel: "Adresse destinataire invalide",
        retryable: false,
        outcomes: [],
      },
    });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText(/ESME_RINVDSTADR/u)).toBeTruthy();
    });

    expect(screen.getByText(/0x0000000B/u)).toBeTruthy();
    expect(screen.getByText(/Adresse destinataire invalide/u)).toBeTruthy();
  });

  /// The lifecycle badge follows `message:update`, not a local guess.
  it("adopts the state the backend pushes rather than deriving one", async () => {
    messageSend.mockResolvedValue({ ok: true, value: anAcceptedResult() });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText(fr.send.state.ACCEPTED)).toBeTruthy();
    });

    useSend.getState().adopt(anAcceptedResult().clientMessageId, "DELIVERED");

    await waitFor(() => {
      expect(screen.getByText(fr.send.state.DELIVERED)).toBeTruthy();
    });
  });

  /// A `message:update` for another message — a campaign at milestone 010 —
  /// must not repaint this panel.
  it("ignores an update about another message", async () => {
    messageSend.mockResolvedValue({ ok: true, value: anAcceptedResult() });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText(fr.send.state.ACCEPTED)).toBeTruthy();
    });

    useSend.getState().adopt("99999999-9999-4999-8999-999999999999", "FAILED");

    expect(screen.getByText(fr.send.state.ACCEPTED)).toBeTruthy();
  });

  /// CA-006-01: the operator watches the message move. The panel therefore
  /// has to appear on the **first** transition, before the command returns —
  /// waiting for the result would collapse the three states into one repaint.
  it("shows the lifecycle badge before the command has returned", async () => {
    render(<SendView />);

    expect(screen.queryByText(fr.send.result.title)).toBeNull();

    useSend.getState().adopt("22222222-2222-4222-8222-222222222222", "QUEUED");

    await waitFor(() => {
      expect(screen.getByText(fr.send.state.QUEUED)).toBeTruthy();
    });
  });

  /// The one state where doing nothing is right and resending is wrong: the
  /// message went out, only its record is missing. If the interface stayed
  /// silent the operator would see an ordinary success and never know.
  it("warns when the message was sent but could not be recorded", async () => {
    messageSend.mockResolvedValue({
      ok: true,
      value: { ...anAcceptedResult(), journalled: false },
    });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText(fr.send.result.notJournalled)).toBeTruthy();
    });
  });

  it("says nothing about the journal when the record went through", async () => {
    messageSend.mockResolvedValue({ ok: true, value: anAcceptedResult() });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(screen.getByText("MSG-42")).toBeTruthy();
    });

    expect(screen.queryByText(fr.send.result.notJournalled)).toBeNull();
  });

  // CA-006-06: what the screen shows is what travels. The sender's type and
  // plan default to "derived", and choosing **one** must not discard it — the
  // field used to grey itself out until its neighbour was set too.
  it("sends each chosen sender type independently of the other", async () => {
    messageSend.mockResolvedValue({ ok: true, value: anAcceptedResult() });

    render(<SendView />);

    await userEvent.selectOptions(screen.getByLabelText(fr.send.session), SESSION);
    await userEvent.type(screen.getByLabelText(fr.send.destination), "+2250102030405");
    await userEvent.type(screen.getByLabelText(fr.send.source), "ShinobiSMS");

    // The default says "derived", and that is what is sent.
    expect((screen.getByLabelText(fr.send.sourceTon) as HTMLSelectElement).value).toBe("");

    // Choosing only the numbering plan leaves the type derived.
    await userEvent.selectOptions(screen.getByLabelText(fr.send.sourceNpi), "isdn");
    await userEvent.click(screen.getByRole("button", { name: fr.send.submit }));

    await waitFor(() => {
      expect(messageSend).toHaveBeenCalled();
    });

    const sent = messageSend.mock.calls[0]?.[0] as MessageSendInput;

    expect(sent.sourceNpi).toBe("isdn");
    expect(sent.sourceTon).toBeNull();
  });

  it("adds and removes a custom optional parameter", async () => {
    render(<SendView />);

    await userEvent.click(screen.getByRole("button", { name: fr.send.tlv.add }));

    expect(screen.getByLabelText(fr.send.tlv.tag)).toBeTruthy();
    expect((screen.getByLabelText(fr.send.tlv.tag) as HTMLInputElement).value).toBe("1403");

    await userEvent.click(screen.getByRole("button", { name: fr.send.tlv.remove }));

    expect(screen.queryByLabelText(fr.send.tlv.tag)).toBeNull();
  });
});
