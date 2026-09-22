// Run by web/tests/messages_render.rs under node: pure rendering, no DOM
// — the /messages page (web UI task 10, "messages") against
// tests/fixtures/messages.json, a fixture of three messages and one
// concierge decision. Checks the three messages render in order, the
// decision shows as a concierge decision, the open question addressed to
// a contact shows blocked, and the message-triggered job shows up; also
// exercises the contact filter and the text search.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  contactsOf, filterMessages, renderMessageRows,
  conciergeDecisions, renderDecisionRows,
  mergeQuestions, renderQuestionRows,
  renderJobRows, renderMessagesDoc,
} = require('../src/messages.js');

const fmtTime = ts => `t${ts}`;

const DOC = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'messages.json'), 'utf8'),
);

// Three messages, in the fixture's own order (newest first) — never
// resorted.
assert.equal(DOC.messages.length, 3);
const rows = renderMessageRows(DOC.messages, fmtTime);
const order = [
  'can you ship the invoice fix today?',
  'the release is queued, should land within the hour',
  'which environment should this go to?',
];
let lastIndex = -1;
for (const text of order) {
  const i = rows.indexOf(text);
  assert.ok(i > lastIndex, `${JSON.stringify(text)} out of order in:\n${rows}`);
  lastIndex = i;
}
assert.equal((rows.match(/class="card msg"/g) || []).length, 3);
assert.match(rows, /from alice/);
assert.match(rows, /to bob/);
assert.match(rows, /href="\/tasks\/41"/);

// The one decision is a concierge decision (`answered_by === "concierge"`)
// and renders as one.
const concierge = conciergeDecisions(DOC.decisions);
assert.equal(concierge.length, 1);
const decisionsHtml = renderDecisionRows(concierge, fmtTime);
assert.equal((decisionsHtml.match(/class="card"/g) || []).length, 1);
assert.match(decisionsHtml, /which environment should this go to\?/);
assert.match(decisionsHtml, /production, same as last time/);

// The open question (still blocked) and the answered one (from the
// decision's own `answered_for`) both show up, each with its own state.
const questions = mergeQuestions(DOC.questions, DOC.decisions);
assert.equal(questions.length, 2);
const questionsHtml = renderQuestionRows(questions);
assert.match(questionsHtml, /should the invoice fix skip the smoke check\?/);
assert.match(questionsHtml, /class="state blocked">blocked/);
assert.match(questionsHtml, /to alice/);

// The job triggered by message 3.
const jobsHtml = renderJobRows(DOC.jobs, fmtTime);
assert.match(jobsHtml, /message 3/);
assert.match(jobsHtml, /quote-by-text/);

// The contact filter: every contact the fixture names, and narrowing to
// one of them keeps only that contact's messages.
assert.deepEqual(contactsOf(DOC.messages), ['alice', 'bob']);
const aliceOnly = filterMessages(DOC.messages, { contact: 'alice' });
assert.equal(aliceOnly.length, 2);
assert.ok(aliceOnly.every(m => m.contact === 'alice'));

// The text search: a case-insensitive substring over the message text.
const smoke = filterMessages(DOC.messages, { q: 'INVOICE' });
assert.equal(smoke.length, 1);
assert.equal(smoke[0].id, 3);
const none = filterMessages(DOC.messages, { q: 'nothing matches this' });
assert.equal(none.length, 0);

// Both filters compose.
const both = filterMessages(DOC.messages, { contact: 'alice', q: 'environment' });
assert.equal(both.length, 1);
assert.equal(both[0].id, 1);

// The whole page renders every section from one merged doc.
const page = renderMessagesDoc(DOC, {}, fmtTime);
assert.match(page, /Messages/);
assert.match(page, /Concierge decisions/);
assert.match(page, /Questions to contacts/);
assert.match(page, /Jobs triggered/);

// Empty inputs render placeholders, not broken sections.
assert.match(renderMessageRows([], fmtTime), /No messages\./);
assert.match(renderDecisionRows([], fmtTime), /No concierge decisions\./);
assert.match(renderQuestionRows([]), /No questions to contacts\./);
assert.match(renderJobRows([], fmtTime), /No jobs triggered by messages\./);
