# Forward commit plan

Each commit is coherent, Conventional, and created only after its focused gates
pass. Exact wording may be tightened, but subsystem boundaries and order remain.

1. `docs(plan): add audited implementation and agent handoff roadmap`
2. `fix(protocol): fail closed unfinished scheduler and integration execution`
3. `fix(acp): preserve credential and filesystem isolation boundaries`
4. `fix(packaging): verify signed acp components before staging`
5. `fix(linux): harden vm broker qualification and transport`
6. `refactor(scheduler): atomically bind occurrences to durable runs`
7. `feat(scheduler): dispatch due runs through qualified isolation`
8. `feat(storage): persist managed integration lifecycle in sqlcipher`
9. `fix(integrations): bind signed manifests to staged bundle identities`
10. `feat(wisp): enable recoverable managed lifecycle operations`
11. `test(qa): add headless product regression coverage`
12. `docs(plan): record verified completion state and external gates`

SuperGrok follows the corrective safety commits as a separate train:

1. `docs(decisions): approve official supergrok api chat contract`
2. `feat(oauth): add daemon-owned supergrok enrollment`
3. `refactor(chat): support explicit credential rails`
4. `feat(chat): add approved supergrok api rail`
5. `feat(ui): expose honest chat rail selection`

The first commit records the owner-authorized, source-pinned public xAI OAuth
contract and intentionally amends the prior direct-API invariant.

Do not commit the present dirty work as one unit. Do not amend reviewed commits,
hand-edit generated code, mix migrations with unrelated UI, or push.
