//! What each repository does with one row, and what it refuses.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use contacts::ports::{ContactRepository, ContactStoreError};
use futures_util::StreamExt;
use messaging::ports::{MessageRepository, MessageStoreError};
use persistence::ports::{
    CampaignRepository, ContactDirectory, MessageJournal, PduLogRepository,
    SessionProfileRepository,
};
use persistence::{
    CampaignId, CampaignStatus, ContactId, Cursor, ListId, ListSelection, MessageFilter,
    MessageState, MessageStateUpdate, PduDirection, PduLogEntry, SqliteCampaignRepository,
    SqliteContactRepository, SqliteMessageRepository, SqlitePduLogRepository,
    SqliteSessionProfileRepository,
};
use smpp_core::types::{ClientMessageId, SessionId};
use smpp_core::values::{CommandStatus, Gsm7BitCharset, Gsm7BitPacking};

use support::{
    a_campaign, a_contact, a_contact_list, a_queued_message, a_session_profile, instant,
    numbered_msisdn, temp_database,
};

// --- Session profiles -------------------------------------------------------

#[tokio::test]
async fn a_session_profile_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqliteSessionProfileRepository::new(harness.database().clone());

    let profile = a_session_profile(SessionId::new(), "staging");
    repository.upsert_session_profile(&profile).await.unwrap();

    let read_back = repository
        .find_session_profile(profile.session_id)
        .await
        .unwrap()
        .expect("the profile was just written");

    assert_eq!(read_back, profile);
}

/// ADR 0009 — the two GSM 7-bit layout settings are stored, not defaulted.
///
/// The fixture uses the defaults, so the round-trip test above would pass just
/// as well if the columns were dropped on the way in and re-defaulted on the
/// way out. This one writes the non-default pair.
#[tokio::test]
async fn the_gsm7_layout_of_a_profile_is_stored_rather_than_defaulted() {
    let harness = temp_database().await;
    let repository = SqliteSessionProfileRepository::new(harness.database().clone());

    let mut profile = a_session_profile(SessionId::new(), "kannel");
    profile.gsm7_charset = Gsm7BitCharset::Latin1;
    profile.gsm7_packing = Gsm7BitPacking::Unpacked;
    repository.upsert_session_profile(&profile).await.unwrap();

    let read_back = repository
        .find_session_profile(profile.session_id)
        .await
        .unwrap()
        .expect("the profile was just written");

    assert_eq!(read_back.gsm7_charset, Gsm7BitCharset::Latin1);
    assert_eq!(read_back.gsm7_packing, Gsm7BitPacking::Unpacked);

    // And the other layout, so neither value is the one that happens to win.
    profile.gsm7_charset = Gsm7BitCharset::Gsm0338;
    profile.gsm7_packing = Gsm7BitPacking::Packed;
    repository.upsert_session_profile(&profile).await.unwrap();

    let read_back = repository
        .find_session_profile(profile.session_id)
        .await
        .unwrap()
        .expect("the profile was just updated");

    assert_eq!(read_back.gsm7_charset, Gsm7BitCharset::Gsm0338);
    assert_eq!(read_back.gsm7_packing, Gsm7BitPacking::Packed);
}

#[tokio::test]
async fn upserting_a_session_profile_twice_updates_it_in_place() {
    let harness = temp_database().await;
    let repository = SqliteSessionProfileRepository::new(harness.database().clone());

    let mut profile = a_session_profile(SessionId::new(), "staging");
    repository.upsert_session_profile(&profile).await.unwrap();

    profile.name = String::from("production");
    profile.throughput_tps = 500;
    profile.updated_at = instant("2026-07-27T09:00:00Z");
    repository.upsert_session_profile(&profile).await.unwrap();

    assert_eq!(repository.list_session_profiles().await.unwrap().len(), 1);
    assert_eq!(
        repository
            .find_session_profile(profile.session_id)
            .await
            .unwrap()
            .unwrap(),
        profile
    );
}

#[tokio::test]
async fn deleting_a_session_profile_reports_whether_it_existed() {
    let harness = temp_database().await;
    let repository = SqliteSessionProfileRepository::new(harness.database().clone());

    let profile = a_session_profile(SessionId::new(), "staging");
    repository.upsert_session_profile(&profile).await.unwrap();

    assert!(repository
        .delete_session_profile(profile.session_id)
        .await
        .unwrap());
    assert!(!repository
        .delete_session_profile(profile.session_id)
        .await
        .unwrap());
}

/// The audit trail outlives the profile: spec §17.6 wants the record of what
/// was sent, and `ON DELETE SET NULL` is what keeps it.
#[tokio::test]
async fn deleting_a_session_profile_detaches_its_messages_without_erasing_them() {
    let harness = temp_database().await;
    let profiles = SqliteSessionProfileRepository::new(harness.database().clone());
    let messages = SqliteMessageRepository::new(harness.database().clone());

    let profile = a_session_profile(SessionId::new(), "staging");
    profiles.upsert_session_profile(&profile).await.unwrap();

    let mut message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    message.session_id = Some(profile.session_id);
    messages.insert_message(&message).await.unwrap();

    profiles
        .delete_session_profile(profile.session_id)
        .await
        .unwrap();

    let read_back = messages
        .find_message(message.client_message_id)
        .await
        .unwrap()
        .expect("the message must survive its session profile");

    assert!(read_back.session_id.is_none());
    assert_eq!(read_back.text, message.text);
}

// --- Contacts ---------------------------------------------------------------

#[tokio::test]
async fn a_contact_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let contact = a_contact(ContactId::new(), "+2250102030405");
    repository.insert_contact(&contact).await.unwrap();

    assert_eq!(
        repository
            .find_contact(contact.contact_id)
            .await
            .unwrap()
            .unwrap(),
        contact
    );
}

#[tokio::test]
async fn inserting_the_same_contact_identifier_twice_is_a_conflict() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let contact = a_contact(ContactId::new(), "+2250102030405");
    repository.insert_contact(&contact).await.unwrap();

    let rejection = repository.insert_contact(&contact).await.unwrap_err();

    assert_eq!(rejection, ContactStoreError::Conflict);
}

/// One transaction for the batch means all-or-nothing: the two valid contacts
/// before the duplicate must not be there afterwards.
#[tokio::test]
async fn a_batch_of_contacts_that_fails_leaves_nothing_behind() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let duplicate = a_contact(ContactId::new(), numbered_msisdn(3).as_str());
    let batch = vec![
        a_contact(ContactId::new(), numbered_msisdn(1).as_str()),
        a_contact(ContactId::new(), numbered_msisdn(2).as_str()),
        duplicate.clone(),
        duplicate,
    ];

    repository.insert_contacts(&batch).await.unwrap_err();

    let page = repository
        .page_contacts(&ListSelection::everything(), None, Cursor::start(), 100)
        .await
        .unwrap();
    assert!(page.is_empty(), "the batch must have rolled back whole");
}

#[tokio::test]
async fn contacts_are_paginated_by_cursor_without_gaps_or_repeats() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let batch: Vec<_> = (0..25)
        .map(|index| a_contact(ContactId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_contacts(&batch).await.unwrap();

    let mut seen = Vec::new();
    let mut cursor = Cursor::start();
    loop {
        let page = repository
            .page_contacts(&ListSelection::everything(), None, cursor, 10)
            .await
            .unwrap();
        seen.extend(page.items.iter().map(|contact| contact.contact_id));

        match page.next {
            Some(next) => cursor = next,
            None => break,
        }
    }

    let expected: Vec<_> = batch.iter().map(|contact| contact.contact_id).collect();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn a_contact_list_holds_only_the_contacts_added_to_it() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let inside = a_contact(ContactId::new(), numbered_msisdn(1).as_str());
    let outside = a_contact(ContactId::new(), numbered_msisdn(2).as_str());
    repository
        .insert_contacts(&[inside.clone(), outside])
        .await
        .unwrap();

    let list = a_contact_list(ListId::new(), "juillet");
    repository.insert_contact_list(&list).await.unwrap();
    let added = repository
        .add_contacts_to_list(list.list_id, &[inside.contact_id])
        .await
        .unwrap();

    assert_eq!(added, 1);

    let members: Vec<_> = repository
        .stream_contacts(&ListSelection::union([list.list_id]))
        .map(|contact| contact.unwrap().contact_id)
        .collect()
        .await;

    assert_eq!(members, vec![inside.contact_id]);
}

#[tokio::test]
async fn adding_a_contact_to_a_list_twice_creates_one_membership() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let contact = a_contact(ContactId::new(), numbered_msisdn(1).as_str());
    repository.insert_contact(&contact).await.unwrap();
    let list = a_contact_list(ListId::new(), "juillet");
    repository.insert_contact_list(&list).await.unwrap();

    let first = repository
        .add_contacts_to_list(list.list_id, &[contact.contact_id])
        .await
        .unwrap();
    let second = repository
        .add_contacts_to_list(list.list_id, &[contact.contact_id])
        .await
        .unwrap();

    assert_eq!((first, second), (1, 0));
}

/// The foreign keys are enforced, not decorative: a membership pointing at a
/// contact that does not exist is refused.
#[tokio::test]
async fn a_membership_referencing_an_unknown_contact_is_refused() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let list = a_contact_list(ListId::new(), "juillet");
    repository.insert_contact_list(&list).await.unwrap();

    let rejection = repository
        .add_contacts_to_list(list.list_id, &[ContactId::new()])
        .await
        .unwrap_err();

    assert_eq!(
        rejection,
        ContactStoreError::NotFound,
        "the foreign key must fire, and the port must name the kind of failure"
    );
}

#[tokio::test]
async fn streaming_without_a_list_yields_every_contact() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let batch: Vec<_> = (0..5)
        .map(|index| a_contact(ContactId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_contacts(&batch).await.unwrap();

    let streamed: Vec<_> = repository
        .stream_contacts(&ListSelection::everything())
        .map(|contact| contact.unwrap().contact_id)
        .collect()
        .await;

    assert_eq!(streamed.len(), batch.len());
}

/// **The traversal order is part of the contract, and nothing was holding it.**
///
/// `messaging::ports::RecipientSource` requires a *stable* order of its
/// implementor, because a resumed campaign re-reads its source from the
/// beginning and a source that reordered itself between two runs makes the
/// progress figures meaningless. `stream_contacts` is that implementor's
/// query, and its `ORDER BY contacts.rowid` was the only thing enforcing it —
/// with no test anywhere: the five streaming tests above assert *membership*,
/// never sequence, so deleting the clause left the whole suite green.
///
/// This asserts insertion order against a traversal that has every reason not
/// to produce it: the numbers descend while the rows ascend, and the membership
/// rows are written back to front, so a plan driven by `contact_list_members`
/// or a sort on any natural key comes out differently.
///
/// # What it catches, measured rather than assumed
///
/// Changing the clause to `ORDER BY contacts.msisdn` — the plausible
/// "improvement" — turns this red. **Deleting it does not**: SQLite's chosen
/// plan for this query scans `contacts` in rowid order anyway, and every test
/// here stays green with no `ORDER BY` at all. That was verified against a live
/// database, not assumed, and it is the honest limit of this test: it pins the
/// order the contract names, and it cannot make the query state it.
#[tokio::test]
async fn streaming_traverses_contacts_in_insertion_order() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    // Numbers DESCEND as the rows are inserted, so "sorted by number" and
    // "insertion order" are opposites rather than coincidences.
    let batch: Vec<_> = (0..20)
        .map(|index| a_contact(ContactId::new(), numbered_msisdn(100 - index).as_str()))
        .collect();
    repository.insert_contacts(&batch).await.unwrap();

    let list = a_contact_list(ListId::new(), "juillet");
    repository.insert_contact_list(&list).await.unwrap();

    // …and the memberships are written back to front, so a join driven by
    // `contact_list_members` would hand them back reversed.
    let members: Vec<_> = batch
        .iter()
        .rev()
        .map(|contact| contact.contact_id)
        .collect();
    repository
        .add_contacts_to_list(list.list_id, &members)
        .await
        .unwrap();

    let expected: Vec<_> = batch.iter().map(|contact| contact.contact_id).collect();

    for selection in [
        ListSelection::everything(),
        ListSelection::union([list.list_id]),
    ] {
        let streamed: Vec<_> = repository
            .stream_contacts(&selection)
            .map(|contact| contact.unwrap().contact_id)
            .collect()
            .await;

        assert_eq!(
            streamed, expected,
            "the traversal order is not the insertion order"
        );
    }
}

/// CA-009-12, against the real SQL rather than against the algebra alone: a
/// union is "in either", an intersection is "in both", and the two give
/// different answers on the same data.
#[tokio::test]
async fn lists_combine_by_union_and_by_intersection() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let only_first = a_contact(ContactId::new(), numbered_msisdn(1).as_str());
    let both = a_contact(ContactId::new(), numbered_msisdn(2).as_str());
    let only_second = a_contact(ContactId::new(), numbered_msisdn(3).as_str());
    let neither = a_contact(ContactId::new(), numbered_msisdn(4).as_str());
    repository
        .insert_contacts(&[
            only_first.clone(),
            both.clone(),
            only_second.clone(),
            neither,
        ])
        .await
        .unwrap();

    let first = a_contact_list(ListId::new(), "juillet");
    let second = a_contact_list(ListId::new(), "abidjan");
    repository.insert_contact_list(&first).await.unwrap();
    repository.insert_contact_list(&second).await.unwrap();
    repository
        .add_contacts_to_list(first.list_id, &[only_first.contact_id, both.contact_id])
        .await
        .unwrap();
    repository
        .add_contacts_to_list(second.list_id, &[only_second.contact_id, both.contact_id])
        .await
        .unwrap();

    let union = ListSelection::union([first.list_id, second.list_id]);
    let intersection = ListSelection::intersection([first.list_id, second.list_id]);

    assert_eq!(repository.count_contacts(&union).await.unwrap(), 3);
    assert_eq!(repository.count_contacts(&intersection).await.unwrap(), 1);

    let members: Vec<_> = repository
        .stream_contacts(&intersection)
        .map(|contact| contact.unwrap().contact_id)
        .collect()
        .await;
    assert_eq!(members, vec![both.contact_id]);
}

/// An exclusion is applied after the combination and cannot be overridden by
/// it — otherwise "everyone in July except the opt-outs" would reach the
/// opt-outs.
#[tokio::test]
async fn an_exclusion_wins_over_the_combination_it_is_applied_to() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let kept = a_contact(ContactId::new(), numbered_msisdn(1).as_str());
    let excluded = a_contact(ContactId::new(), numbered_msisdn(2).as_str());
    repository
        .insert_contacts(&[kept.clone(), excluded.clone()])
        .await
        .unwrap();

    let campaign = a_contact_list(ListId::new(), "juillet");
    let optout = a_contact_list(ListId::new(), "opt-out");
    repository.insert_contact_list(&campaign).await.unwrap();
    repository.insert_contact_list(&optout).await.unwrap();
    repository
        .add_contacts_to_list(campaign.list_id, &[kept.contact_id, excluded.contact_id])
        .await
        .unwrap();
    repository
        .add_contacts_to_list(optout.list_id, &[excluded.contact_id])
        .await
        .unwrap();

    let selection = ListSelection::union([campaign.list_id]).excluding([optout.list_id]);

    let members: Vec<_> = repository
        .stream_contacts(&selection)
        .map(|contact| contact.unwrap().contact_id)
        .collect()
        .await;

    assert_eq!(members, vec![kept.contact_id]);
    assert_eq!(repository.count_contacts(&selection).await.unwrap(), 1);
}

/// The trap the algebra exists to prevent, asserted against the SQL rather
/// than only against the type: an empty intersection must select NOTHING.
/// `COUNT(…) = 0` over no list is trivially true of every contact, so an
/// implementation that let the empty case reach the query would return the
/// whole table here.
#[tokio::test]
async fn an_empty_combination_selects_no_contact_rather_than_all_of_them() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let batch: Vec<_> = (0..5)
        .map(|index| a_contact(ContactId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_contacts(&batch).await.unwrap();

    for empty in [
        ListSelection::union(Vec::new()),
        ListSelection::intersection(Vec::new()),
    ] {
        assert_eq!(repository.count_contacts(&empty).await.unwrap(), 0);
        assert_eq!(
            repository
                .stream_contacts(&empty)
                .collect::<Vec<_>>()
                .await
                .len(),
            0
        );
        assert!(repository
            .page_contacts(&empty, None, Cursor::start(), 10)
            .await
            .unwrap()
            .is_empty());
    }

    assert_eq!(
        repository
            .count_contacts(&ListSelection::everything())
            .await
            .unwrap(),
        5,
        "…while `everything` still means everything"
    );
}

/// The contacts screen searches on the number and on the attributes, and on
/// nothing else: an operator types digits or a name, never `import_csv`.
#[tokio::test]
async fn a_page_can_be_searched_by_number_and_by_attribute() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let batch: Vec<_> = (0..3)
        .map(|index| a_contact(ContactId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_contacts(&batch).await.unwrap();

    let by_number = repository
        .page_contacts(
            &ListSelection::everything(),
            Some(numbered_msisdn(1).as_str()),
            Cursor::start(),
            10,
        )
        .await
        .unwrap();
    assert_eq!(by_number.len(), 1);

    let by_attribute = repository
        .page_contacts(
            &ListSelection::everything(),
            Some("Awa"),
            Cursor::start(),
            10,
        )
        .await
        .unwrap();
    assert_eq!(by_attribute.len(), 3);

    // "fixture" IS the `source` of every contact above, so a search that
    // covered that column would return three here. Searching for a value the
    // fixtures do not hold would pass whatever the query did.
    let by_source = repository
        .page_contacts(
            &ListSelection::everything(),
            Some("fixture"),
            Cursor::start(),
            10,
        )
        .await
        .unwrap();
    assert!(
        by_source.is_empty(),
        "the source column is not part of the search"
    );
}

/// CA-009-09: a saved profile comes back with the mapping it was saved with,
/// and saving it again replaces it rather than adding a second row.
#[tokio::test]
async fn an_import_profile_survives_a_round_trip_and_is_replaced_in_place() {
    use contacts::import::{ColumnMapping, ColumnRef, ImportProfile};
    use persistence::ProfileId;

    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let profile = ImportProfile {
        profile_id: ProfileId::new(),
        name: String::from("fichier client"),
        mapping: ColumnMapping::by_name("telephone")
            .with_country(ColumnRef::Name(String::from("pays")))
            .with_attribute("prenom", ColumnRef::Index(2)),
        created_at: instant("2026-07-27T09:00:00Z"),
    };

    repository.upsert_import_profile(&profile).await.unwrap();

    assert_eq!(
        repository.list_import_profiles().await.unwrap(),
        vec![profile.clone()]
    );

    let renamed = ImportProfile {
        name: String::from("fichier client v2"),
        ..profile
    };
    repository.upsert_import_profile(&renamed).await.unwrap();

    assert_eq!(
        repository.list_import_profiles().await.unwrap(),
        vec![renamed],
        "an upsert replaces rather than adding a second row"
    );
}

// --- Campaigns --------------------------------------------------------------

#[tokio::test]
async fn a_campaign_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqliteCampaignRepository::new(harness.database().clone());

    let campaign = a_campaign(CampaignId::new(), "juillet");
    repository.upsert_campaign(&campaign).await.unwrap();

    assert_eq!(
        repository
            .find_campaign(campaign.campaign_id)
            .await
            .unwrap()
            .unwrap(),
        campaign
    );
}

#[tokio::test]
async fn a_campaign_status_change_is_persisted() {
    let harness = temp_database().await;
    let repository = SqliteCampaignRepository::new(harness.database().clone());

    let mut campaign = a_campaign(CampaignId::new(), "juillet");
    repository.upsert_campaign(&campaign).await.unwrap();

    campaign.status = CampaignStatus::Running;
    campaign.started_at = Some(instant("2026-07-26T11:00:00Z"));
    campaign.total_count = 1_000;
    repository.upsert_campaign(&campaign).await.unwrap();

    let read_back = repository
        .find_campaign(campaign.campaign_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(read_back.status, CampaignStatus::Running);
    assert_eq!(read_back.started_at, campaign.started_at);
    assert_eq!(read_back.total_count, 1_000);
}

#[tokio::test]
async fn deleting_a_campaign_detaches_its_messages_without_erasing_them() {
    let harness = temp_database().await;
    let campaigns = SqliteCampaignRepository::new(harness.database().clone());
    let messages = SqliteMessageRepository::new(harness.database().clone());

    let campaign = a_campaign(CampaignId::new(), "juillet");
    campaigns.upsert_campaign(&campaign).await.unwrap();

    let mut message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    message.campaign_id = Some(campaign.campaign_id);
    messages.insert_message(&message).await.unwrap();

    assert!(campaigns
        .delete_campaign(campaign.campaign_id)
        .await
        .unwrap());

    let read_back = messages
        .find_message(message.client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert!(read_back.campaign_id.is_none());
}

// --- Messages ---------------------------------------------------------------

#[tokio::test]
async fn a_message_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    repository.insert_message(&message).await.unwrap();

    assert_eq!(
        repository
            .find_message(message.client_message_id)
            .await
            .unwrap()
            .unwrap(),
        message
    );
}

#[tokio::test]
async fn inserting_the_same_client_message_identifier_twice_is_a_conflict() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    repository.insert_message(&message).await.unwrap();

    let rejection = repository.insert_message(&message).await.unwrap_err();

    assert!(
        rejection == MessageStoreError::Conflict,
        "expected a conflict, got {rejection:?}"
    );
}

/// The whole lifecycle of spec §14.3, one transition at a time, checking that
/// each one keeps what the previous one wrote.
#[tokio::test]
async fn the_lifecycle_transitions_accumulate_rather_than_overwrite() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    let identifier = message.client_message_id;
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Sent)
                .sent_at(instant("2026-07-26T10:00:01Z"), 1),
        )
        .await
        .unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Accepted)
                .with_smsc_message_id("SMSC-1")
                .with_command_status(CommandStatus::EsmeRok)
                .responded_at(instant("2026-07-26T10:00:02Z")),
        )
        .await
        .unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Delivered)
                .with_delivery_receipt("DELIVRD", None)
                .receipt_at(instant("2026-07-26T10:00:30Z")),
        )
        .await
        .unwrap();

    let read_back = repository.find_message(identifier).await.unwrap().unwrap();

    assert_eq!(read_back.state, MessageState::Delivered);
    assert_eq!(read_back.smsc_message_id.as_deref(), Some("SMSC-1"));
    assert_eq!(read_back.command_status, Some(CommandStatus::EsmeRok));
    assert_eq!(read_back.dlr_stat.as_deref(), Some("DELIVRD"));
    assert_eq!(read_back.sent_at, Some(instant("2026-07-26T10:00:01Z")));
    assert_eq!(read_back.resp_at, Some(instant("2026-07-26T10:00:02Z")));
    assert_eq!(read_back.dlr_at, Some(instant("2026-07-26T10:00:30Z")));
    assert_eq!(read_back.attempts, 1, "only the send counts as an attempt");
}

/// Replaying a transition must be harmless (CLAUDE.md §4).
///
/// # Non-regression
///
/// The first version of this test used `.responded_at(…)` — the one builder
/// that touches no counter — and passed while `update_state` incremented
/// `attempts` on every application. It now goes through `.sent_at(…)`, the
/// transition that actually carries a counter, which is the only shape where
/// idempotence is not free. `attempts` is asserted explicitly rather than only
/// through the whole-record comparison, so a future change that reintroduces
/// an increment fails on the line that names it.
#[tokio::test]
async fn replaying_a_send_transition_leaves_the_row_unchanged() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    let identifier = message.client_message_id;
    repository.insert_message(&message).await.unwrap();

    let transition = MessageStateUpdate::new(identifier, MessageState::Sent)
        .sent_at(instant("2026-07-26T10:00:01Z"), 1);

    repository.update_state(&transition).await.unwrap();
    let once = repository.find_message(identifier).await.unwrap().unwrap();

    repository.update_state(&transition).await.unwrap();
    repository.update_state(&transition).await.unwrap();
    let thrice = repository.find_message(identifier).await.unwrap().unwrap();

    assert_eq!(once.attempts, 1);
    assert_eq!(thrice.attempts, 1, "the attempt counter was incremented");
    assert_eq!(once, thrice);
}

/// The same, one level up: a whole batch replayed after a crash.
///
/// This is the shape the failure actually takes (spec §10.5) — `update_states`
/// commits, the process dies before the in-memory window is cleared, and the
/// resumed run reapplies the batch.
#[tokio::test]
async fn replaying_a_committed_batch_does_not_inflate_the_attempt_counters() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let messages: Vec<_> = (0..5)
        .map(|index| a_queued_message(ClientMessageId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_messages(&messages).await.unwrap();

    let batch: Vec<MessageStateUpdate> = messages
        .iter()
        .map(|message| {
            MessageStateUpdate::new(message.client_message_id, MessageState::Sent)
                .sent_at(instant("2026-07-26T10:00:01Z"), 1)
        })
        .collect();

    repository.update_states(&batch).await.unwrap();
    repository.update_states(&batch).await.unwrap();

    for message in &messages {
        let read_back = repository
            .find_message(message.client_message_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(read_back.attempts, 1);
    }
}

/// A genuine second attempt does move the counter — the point is idempotence,
/// not immobility.
#[tokio::test]
async fn a_second_attempt_advances_the_counter() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    let identifier = message.client_message_id;
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Sent)
                .sent_at(instant("2026-07-26T10:00:01Z"), 1),
        )
        .await
        .unwrap();
    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Sent)
                .sent_at(instant("2026-07-26T10:00:31Z"), 2),
        )
        .await
        .unwrap();

    let read_back = repository.find_message(identifier).await.unwrap().unwrap();

    assert_eq!(read_back.attempts, 2);
}

/// A retried send gets a **new** SMSC identifier, and it must win.
///
/// Spec §10.7: the first `submit_sm` times out and is retried; the SMSC
/// assigns a fresh identifier to the retry. If the late response to the first
/// attempt lands first, the retry's response has to overwrite it — otherwise
/// the retry's delivery receipt never correlates (spec §7.8) and the message
/// sits in `ACCEPTED` for ever.
#[tokio::test]
async fn a_later_smsc_identifier_replaces_an_earlier_one() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    let identifier = message.client_message_id;
    repository.insert_message(&message).await.unwrap();

    // The late response to attempt 1.
    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Accepted)
                .with_smsc_message_id("SMSC-first"),
        )
        .await
        .unwrap();

    // The response to attempt 2, carrying the identifier the SMSC really used.
    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Accepted)
                .with_smsc_message_id("SMSC-second"),
        )
        .await
        .unwrap();

    let read_back = repository.find_message(identifier).await.unwrap().unwrap();
    assert_eq!(read_back.smsc_message_id.as_deref(), Some("SMSC-second"));

    // And the delivery receipt correlates against the current identifier.
    let correlated = repository
        .find_message_by_smsc_id("SMSC-second", None)
        .await
        .unwrap()
        .expect("the receipt must find its message");
    assert_eq!(correlated.client_message_id, identifier);
}

/// A transition that carries no identifier leaves the one already stored.
#[tokio::test]
async fn a_transition_without_an_identifier_keeps_the_stored_one() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    let identifier = message.client_message_id;
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Accepted)
                .with_smsc_message_id("SMSC-1"),
        )
        .await
        .unwrap();
    repository
        .update_state(
            &MessageStateUpdate::new(identifier, MessageState::Delivered)
                .with_delivery_receipt("DELIVRD", None),
        )
        .await
        .unwrap();

    let read_back = repository.find_message(identifier).await.unwrap().unwrap();

    assert_eq!(read_back.smsc_message_id.as_deref(), Some("SMSC-1"));
    assert_eq!(read_back.state, MessageState::Delivered);
}

#[tokio::test]
async fn a_transition_on_an_unknown_message_is_not_found() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let rejection = repository
        .update_state(&MessageStateUpdate::new(
            ClientMessageId::new(),
            MessageState::Sent,
        ))
        .await
        .unwrap_err();

    assert!(
        rejection == MessageStoreError::NotFound,
        "expected a not-found, got {rejection:?}"
    );
}

#[tokio::test]
async fn a_message_is_found_by_the_identifier_the_smsc_assigned() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    repository.insert_message(&message).await.unwrap();
    repository
        .update_state(
            &MessageStateUpdate::new(message.client_message_id, MessageState::Accepted)
                .with_smsc_message_id("SMSC-42"),
        )
        .await
        .unwrap();

    let found = repository
        .find_message_by_smsc_id("SMSC-42", None)
        .await
        .unwrap()
        .expect("the delivery receipt must find its message");

    assert_eq!(found.client_message_id, message.client_message_id);
    assert!(repository
        .find_message_by_smsc_id("SMSC-unknown", None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn messages_are_filtered_by_campaign_and_by_state() {
    let harness = temp_database().await;
    let campaigns = SqliteCampaignRepository::new(harness.database().clone());
    let messages = SqliteMessageRepository::new(harness.database().clone());

    let campaign = a_campaign(CampaignId::new(), "juillet");
    campaigns.upsert_campaign(&campaign).await.unwrap();

    let mut inside = a_queued_message(ClientMessageId::new(), numbered_msisdn(1).as_str());
    inside.campaign_id = Some(campaign.campaign_id);
    let outside = a_queued_message(ClientMessageId::new(), numbered_msisdn(2).as_str());
    messages
        .insert_messages(&[inside.clone(), outside])
        .await
        .unwrap();

    messages
        .update_state(&MessageStateUpdate::new(
            inside.client_message_id,
            MessageState::Sent,
        ))
        .await
        .unwrap();

    let by_campaign = MessageFilter::all().for_campaign(campaign.campaign_id);
    assert_eq!(messages.count_messages(&by_campaign).await.unwrap(), 1);

    let queued = MessageFilter::all().in_state(MessageState::Queued);
    assert_eq!(messages.count_messages(&queued).await.unwrap(), 1);

    let sent_in_campaign = MessageFilter::all()
        .for_campaign(campaign.campaign_id)
        .in_state(MessageState::Sent);
    let page = messages
        .page_messages(&sent_in_campaign, Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(page.len(), 1);
    assert_eq!(page.items[0].client_message_id, inside.client_message_id);
}

#[tokio::test]
async fn an_empty_filter_counts_every_message() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let batch: Vec<_> = (0..7)
        .map(|index| a_queued_message(ClientMessageId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_messages(&batch).await.unwrap();

    assert_eq!(
        repository
            .count_messages(&MessageFilter::all())
            .await
            .unwrap(),
        7
    );
}

// --- PDU log ----------------------------------------------------------------

#[tokio::test]
async fn a_pdu_log_entry_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqlitePduLogRepository::new(harness.database().clone());

    let entry = PduLogEntry {
        session_id: Some(SessionId::new()),
        direction: PduDirection::Outbound,
        command_id: Some(0x0000_0004),
        command_status: Some(0),
        sequence_number: Some(1),
        raw_hex: Some(String::from("0000001F")),
        decoded: Some(String::from("submit_sm")),
        ts: instant("2026-07-26T10:00:00Z"),
    };

    assert!(repository.insert_entry(&entry).await.unwrap() > 0);

    let page = repository
        .page_entries(None, Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(
        page.items
            .into_iter()
            .map(|row| row.entry)
            .collect::<Vec<_>>(),
        vec![entry]
    );
}

#[tokio::test]
async fn pdu_log_entries_are_filtered_by_session() {
    let harness = temp_database().await;
    let repository = SqlitePduLogRepository::new(harness.database().clone());

    let wanted = SessionId::new();
    for session_id in [Some(wanted), Some(SessionId::new()), None] {
        repository
            .insert_entry(&PduLogEntry {
                session_id,
                direction: PduDirection::Inbound,
                command_id: None,
                command_status: None,
                sequence_number: None,
                raw_hex: None,
                decoded: None,
                ts: instant("2026-07-26T10:00:00Z"),
            })
            .await
            .unwrap();
    }

    let page = repository
        .page_entries(Some(wanted), Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(page.len(), 1);
    assert_eq!(page.items[0].entry.session_id, Some(wanted));
}
