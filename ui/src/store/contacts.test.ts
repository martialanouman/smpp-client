import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ContactPageDto, ContactRowDto } from "../ipc";
import { useContacts } from "./contacts";

const contactsPage = vi.fn();
const contactsImport = vi.fn();
const contactsLists = vi.fn();
const contactsProfiles = vi.fn();

vi.mock("../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc")>()),
  contactsPage: (...args: unknown[]) => contactsPage(...args) as unknown,
  contactsImport: (...args: unknown[]) => contactsImport(...args) as unknown,
  contactsLists: () => contactsLists() as unknown,
  contactsProfiles: () => contactsProfiles() as unknown,
}));

/** One row, identified by its number so a duplicate is visible in an assertion. */
function aRow(msisdn: string): ContactRowDto {
  return {
    contactId: `id-${msisdn}`,
    msisdn,
    country: "CI",
    valid: true,
    lineType: "mobile",
    attributes: null,
    source: "import_csv",
    createdAt: "2026-08-02T10:00:00Z",
  };
}

/** A successful page. */
function aPage(rows: ContactRowDto[], next: string | null, total = rows.length) {
  return { ok: true as const, value: { rows, next, total } satisfies ContactPageDto };
}

describe("the contacts store", () => {
  beforeEach(() => {
    contactsPage.mockReset();
    contactsImport.mockReset();
    contactsLists.mockReset().mockResolvedValue({ ok: true, value: [] });
    contactsProfiles.mockReset().mockResolvedValue({ ok: true, value: [] });

    useContacts.setState({
      rows: [],
      total: 0,
      cursor: null,
      loading: false,
      complete: false,
      selection: { combination: "everything", lists: [], excluded: [] },
      search: "",
      lists: [],
      profiles: [],
      importing: false,
      progress: null,
      report: null,
    });
  });

  it("appends the next page instead of replacing what is held", async () => {
    contactsPage
      .mockResolvedValueOnce(aPage([aRow("+2250700000001")], "10", 2))
      .mockResolvedValueOnce(aPage([aRow("+2250700000002")], null, 2));

    await useContacts.getState().reload();
    await useContacts.getState().loadMore();

    expect(useContacts.getState().rows.map((row) => row.msisdn)).toEqual([
      "+2250700000001",
      "+2250700000002",
    ]);
    expect(useContacts.getState().complete).toBe(true);
  });

  /**
   * The guard that matters most. A virtualised table fires `onScroll` many
   * times per gesture, and each firing calls `loadMore`. Without the `loading`
   * guard every one of them issues the same cursor and the rows arrive twice —
   * which shows up as duplicated contacts, not as an error.
   */
  it("refuses a second page request while one is in flight", async () => {
    contactsPage.mockResolvedValueOnce(aPage([aRow("+2250700000001")], "10", 2));
    await useContacts.getState().reload();

    let release: (value: unknown) => void = () => undefined;
    contactsPage.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      }),
    );

    const first = useContacts.getState().loadMore();
    await useContacts.getState().loadMore();
    await useContacts.getState().loadMore();

    release(aPage([aRow("+2250700000002")], null, 2));
    await first;

    // Four calls attempted, two reached the backend: the reload and one page.
    expect(contactsPage).toHaveBeenCalledTimes(2);
    expect(useContacts.getState().rows).toHaveLength(2);
  });

  it("stops asking once a page comes back without a cursor", async () => {
    contactsPage.mockResolvedValueOnce(aPage([aRow("+2250700000001")], null, 1));

    await useContacts.getState().reload();
    await useContacts.getState().loadMore();

    expect(contactsPage).toHaveBeenCalledTimes(1);
  });

  /**
   * Changing the search must clear the rows **before** the new page arrives,
   * not after. Asserting only the final state would pass either way, since
   * `reload` replaces the rows regardless; what the clearing buys is the
   * interval in between, during which a table that kept them would be showing
   * contacts the operator has just excluded.
   */
  it("clears the rows while the new search is still in flight", async () => {
    contactsPage.mockResolvedValueOnce(aPage([aRow("+2250700000001")], "10", 2));
    await useContacts.getState().reload();

    let release: (value: unknown) => void = () => undefined;
    contactsPage.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      }),
    );

    const pending = useContacts.getState().setSearch("9");

    expect(useContacts.getState().rows).toEqual([]);
    expect(useContacts.getState().cursor).toBeNull();

    release(aPage([aRow("+2250700000009")], null, 1));
    await pending;

    expect(useContacts.getState().rows.map((row) => row.msisdn)).toEqual(["+2250700000009"]);
    expect(contactsPage).toHaveBeenLastCalledWith(
      { combination: "everything", lists: [], excluded: [] },
      "9",
      null,
      100,
    );
  });

  /**
   * The race the `loading` guard on `reload` used to lose. An operator typing
   * "ab" fires two searches; if the second is dropped because the first is
   * still in flight, the screen ends up showing the results of "a" under the
   * text "ab" — and, worse, no further request is ever issued, so the table
   * stays wrong until they retype.
   *
   * The guard belongs on `loadMore`, which appends, and not on `reload`, which
   * replaces.
   */
  it("issues the second search even while the first is in flight", async () => {
    let releaseFirst: (value: unknown) => void = () => undefined;
    contactsPage
      .mockReturnValueOnce(
        new Promise((resolve) => {
          releaseFirst = resolve;
        }),
      )
      .mockResolvedValueOnce(aPage([aRow("+2250700000009")], null, 1));

    const first = useContacts.getState().setSearch("a");
    const second = useContacts.getState().setSearch("ab");

    // The stale answer lands after the fresh one was already asked for.
    releaseFirst(aPage([aRow("+2250700000001")], "10", 2));
    await Promise.all([first, second]);

    expect(contactsPage).toHaveBeenCalledTimes(2);
    expect(useContacts.getState().rows.map((row) => row.msisdn)).toEqual(["+2250700000009"]);
    expect(useContacts.getState().search).toBe("ab");
  });

  /**
   * CA-009-10. A cancelled import still wrote the batches it committed, so the
   * table has to be refreshed whatever the outcome — including when the import
   * came back as a failure.
   */
  it("reloads the table after an import that failed", async () => {
    contactsImport.mockResolvedValue({
      ok: false,
      failure: { kind: "backend", error: { code: "CONTACTS_STORAGE", message: "", details: null } },
    });
    contactsPage.mockResolvedValue(aPage([aRow("+2250700000001")], null, 1));

    await useContacts.getState().runImport(
      { kind: "csv", path: "/tmp/contacts.csv" },
      {
        mapping: { msisdn: { by: "name", value: "msisdn" }, country: null, attributes: [] },
        headers: "detect",
        defaultRegion: null,
        mobilesOnly: false,
        deduplication: "firstWins",
        listId: null,
      },
    );

    expect(contactsPage).toHaveBeenCalled();
    expect(useContacts.getState().importing).toBe(false);
    expect(useContacts.getState().report).toBeNull();
  });
});
