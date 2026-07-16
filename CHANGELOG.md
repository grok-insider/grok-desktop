# Changelog

## [0.0.8](https://github.com/grok-insider/grok-desktop/compare/v0.0.7...v0.0.8) (2026-07-16)


### Bug Fixes

* **release:** allow verified DigiCert timestamp endpoint ([#34](https://github.com/grok-insider/grok-desktop/issues/34)) ([1aac342](https://github.com/grok-insider/grok-desktop/commit/1aac342ebfb67015a633eff8d2ac2b5883a30256))

## [0.0.7](https://github.com/grok-insider/grok-desktop/compare/v0.0.6...v0.0.7) (2026-07-16)

### Highlights

- Fixed Electron packager metadata to ensure consistent builds.


### Bug Fixes

* **release:** pin Electron packager metadata ([#31](https://github.com/grok-insider/grok-desktop/issues/31)) ([17ab5e8](https://github.com/grok-insider/grok-desktop/commit/17ab5e8f9f908e1d5634bcf2adcef862cb5e6cb6))

## [0.0.6](https://github.com/grok-insider/grok-desktop/compare/v0.0.5...v0.0.6) (2026-07-16)

### Highlights

- Fixed a build failure that could occur on Windows during runtime packaging.


### Bug Fixes

* **release:** fail closed on Windows runtime build ([#28](https://github.com/grok-insider/grok-desktop/issues/28)) ([8f8e3ce](https://github.com/grok-insider/grok-desktop/commit/8f8e3cec15dd91a294d5cab3aa1a309942d303f0))

## [0.0.5](https://github.com/grok-insider/grok-desktop/compare/v0.0.4...v0.0.5) (2026-07-16)


### Bug Fixes

* **release:** clean Windows core staging ([856adbd](https://github.com/grok-insider/grok-desktop/commit/856adbda142d73913e1c989693e26df7cdb109d3))
* **release:** clean Windows core staging ([f220bcb](https://github.com/grok-insider/grok-desktop/commit/f220bcb5eba6c6a83e19092c57f4430cfd198fc5))

## [0.0.4](https://github.com/grok-insider/grok-desktop/compare/v0.0.3...v0.0.4) (2026-07-16)

### Highlights

- Fixed AppStream metadata for Linux package compatibility
- Fixed Windows package options forwarding


### Bug Fixes

* **release:** forward Windows package options ([d35da9b](https://github.com/grok-insider/grok-desktop/commit/d35da9bbf39e1a600e6787544ffb80454ca3dd3b))
* **release:** forward Windows package options ([b0f4c27](https://github.com/grok-insider/grok-desktop/commit/b0f4c278d4fe4df33379d5c5287ce40ef2e312a6))
* **release:** provide valid AppStream metadata ([4e6bc13](https://github.com/grok-insider/grok-desktop/commit/4e6bc133f7c85386b5ab976d93b343cf6730a0be))
* **release:** provide valid AppStream metadata ([fe873cf](https://github.com/grok-insider/grok-desktop/commit/fe873cf0e49f2d49dded7539b7ee673a61648d7f))

## [0.0.3](https://github.com/grok-insider/grok-desktop/compare/v0.0.2...v0.0.3) (2026-07-16)


### Bug Fixes

* **release:** bind certificate ACL to runner SID ([5230afe](https://github.com/grok-insider/grok-desktop/commit/5230afe1456fad52ec4a89796af8df435d0e0284))
* **release:** bind certificate ACL to runner SID ([2b9609d](https://github.com/grok-insider/grok-desktop/commit/2b9609d0e0e3fe43a3f9332b7c121f3fca092e18))
* **release:** promote artifact packaging repairs ([2e1c636](https://github.com/grok-insider/grok-desktop/commit/2e1c636873cb7d8c3bb4dea731b10e44c614bf8d))
* **release:** resolve package-relative manifests ([dc3bba2](https://github.com/grok-insider/grok-desktop/commit/dc3bba2d250a67cca601d8726ec04ecba0c6b047))

## [0.0.2](https://github.com/grok-insider/grok-desktop/compare/v0.0.1...v0.0.2) (2026-07-16)


### Bug Fixes

* **release:** finalize Release Please lifecycle ([87873ef](https://github.com/grok-insider/grok-desktop/commit/87873ef8efe14ae2273ec755a7929620f843c283))
* **release:** finalize release-please lifecycle ([711f4f4](https://github.com/grok-insider/grok-desktop/commit/711f4f4e95ea5b663c1a2d5001e7decf0bbf6016))
* **release:** pin immutable AppImage tooling ([73e4aad](https://github.com/grok-insider/grok-desktop/commit/73e4aaddd24c0e82e50665463ff3ddb9104d4065))
* **release:** pin immutable AppImage tooling ([b433211](https://github.com/grok-insider/grok-desktop/commit/b4332119525eca11de28af1502f3fcaec38f7139))
* **release:** promote finalizer lifecycle repair ([c59edf5](https://github.com/grok-insider/grok-desktop/commit/c59edf5b9584ccb57dc64f4046600dc6611d45e5))

## 0.0.1 (2026-07-16)


### Features

* **acp:** add exclusive host work runtime roles ([74578ad](https://github.com/grok-insider/grok-desktop/commit/74578adc713016f8238208305781c8d411944b46))
* **acp:** add Grok Build ACP integration crate ([3dec46b](https://github.com/grok-insider/grok-desktop/commit/3dec46b9560b9c4fadde5ced95118508087fbdf6))
* **application:** add conversation and credentials use cases ([baba96f](https://github.com/grok-insider/grok-desktop/commit/baba96f53a6a41619e823825ad4a3431fa3e7f56))
* **application:** add ports, models, and error types ([6d97529](https://github.com/grok-insider/grok-desktop/commit/6d975291c56db8cde428eef5a7924b90142acfff))
* **application:** add runs, approvals, and effects ([64bd1e9](https://github.com/grok-insider/grok-desktop/commit/64bd1e926c48da96cb6a40582bd0d26c4eec256e))
* **application:** add workspace, artifacts, isolation, automation ([0573df4](https://github.com/grok-insider/grok-desktop/commit/0573df465f53df48413cde1cd6d1fa5698adcd0c))
* **artifact-storage:** add private artifact storage adapter ([c2c4a14](https://github.com/grok-insider/grok-desktop/commit/c2c4a148d4b80dcdd4a38dd833330f4e7fafa83d))
* **chat:** add daemon-owned product identity prompt ([129c7f6](https://github.com/grok-insider/grok-desktop/commit/129c7f62a3d3f68347b5f7685ace4022cadd8279))
* **chat:** add durable official xai search ([10e8a86](https://github.com/grok-insider/grok-desktop/commit/10e8a865ab40adbb263ae989dda8ba236ede5cfb))
* **chat:** add fixed-origin oauth responses adapter ([8b96387](https://github.com/grok-insider/grok-desktop/commit/8b9638776b5c4ff416ae791de043843d6c26d151))
* **chat:** bind model selection to conversations ([80f9e0d](https://github.com/grok-insider/grok-desktop/commit/80f9e0de8fd3f3af1080e26483c0b86f7b0bc367))
* **chat:** clarify daemon-owned capability prompt ([023318d](https://github.com/grok-insider/grok-desktop/commit/023318d8dcff1365cdf9e9fe763639cb5e0f0e2f))
* **chat:** connect daemon-owned supergrok api rail ([1342368](https://github.com/grok-insider/grok-desktop/commit/13423680ded67dc89b534ea43a5a09eb24b8450f))
* **chat:** follow durable streaming output ([57c6528](https://github.com/grok-insider/grok-desktop/commit/57c6528763deed52e93207bb2b53da1a1530bd67))
* **credential-enrollment:** add native enrollment boundary ([4bb82cc](https://github.com/grok-insider/grok-desktop/commit/4bb82cc4fa6f847c343671874ddf9cfea383689c))
* **daemon:** add daemon IPC host and transport ([4e3639a](https://github.com/grok-insider/grok-desktop/commit/4e3639a2f1028daa8d5f3e52ac1808b7942ecd9a))
* **daemon:** add durable scheduler and integration journals ([49eec5c](https://github.com/grok-insider/grok-desktop/commit/49eec5c79feb95926d23f87f03c2f40473a165e9))
* **daemon:** add recovery and IPC tests; lock workspace ([3b01341](https://github.com/grok-insider/grok-desktop/commit/3b013418d87dc4f044ddab3e668b936427154ca1))
* **daemon:** persist explicit host tools enrollment ([6b571f6](https://github.com/grok-insider/grok-desktop/commit/6b571f64d3b03f938e9737b8ccdb23c6a9376660))
* **desktop:** add CDP and Windows build scripts ([ed65b16](https://github.com/grok-insider/grok-desktop/commit/ed65b16dfc52df422fe1342df3ae8dd2d2684b4b))
* **desktop:** add daemon supervision and RPC client ([d403db7](https://github.com/grok-insider/grok-desktop/commit/d403db7baa82491b35b8728c1ecc7474dcf6624b))
* **desktop:** add Electron main, preload, and shell helpers ([e08e86c](https://github.com/grok-insider/grok-desktop/commit/e08e86cf1cdb7cc0e30221365c54616cb1fee0b7))
* **desktop:** add generated IPC bindings ([081f187](https://github.com/grok-insider/grok-desktop/commit/081f187befa34a5bfbcef4134f26694f40a058df))
* **desktop:** add honest application update controls ([f0b53a2](https://github.com/grok-insider/grok-desktop/commit/f0b53a2929610693eeedd18283d570cfbe2db701))
* **desktop:** add Imagine image and video as composer tools ([621763a](https://github.com/grok-insider/grok-desktop/commit/621763a3fb971df26c84f2b8c3c8c266b7adce6e))
* **desktop:** add main-process security policies ([afd5c41](https://github.com/grok-insider/grok-desktop/commit/afd5c4159da2ec37ba0d7e5771d09213b3e0a998))
* **desktop:** add product views ([f663090](https://github.com/grok-insider/grok-desktop/commit/f663090fabc30282c5befc800122307f56bf1bd6))
* **desktop:** add renderer bridge, services, and styles ([f587e1b](https://github.com/grok-insider/grok-desktop/commit/f587e1b920f890e076da3257e98cc6abd11216c0))
* **desktop:** add supergrok api chat setup flow ([2642f46](https://github.com/grok-insider/grok-desktop/commit/2642f464e9a74ed09bb30a5ee8ae8a0a6d14414c))
* **desktop:** add tray assets and Windows packaging templates ([49c7b79](https://github.com/grok-insider/grok-desktop/commit/49c7b7990a56134d2224844675310a02fa9210e8))
* **desktop:** add UI primitives and composer ([4cf2a0f](https://github.com/grok-insider/grok-desktop/commit/4cf2a0f4fffa85089794090f634b1fa385166be8))
* **desktop:** adopt command palette and sheet overlays ([d4ea432](https://github.com/grok-insider/grok-desktop/commit/d4ea4320e82fb4e166867496d644a458f7c5fb73))
* **desktop:** adopt shadcn alert, progress, and kbd primitives ([0e30ad4](https://github.com/grok-insider/grok-desktop/commit/0e30ad46d9110f074dfa548c3742ce46529ec1c6))
* **desktop:** adopt shadcn menus and selects in chat surfaces ([7621062](https://github.com/grok-insider/grok-desktop/commit/7621062ed49102609ba2168e7982d8600ee54b90))
* **desktop:** expose explicit host tools work mode ([18b3c0a](https://github.com/grok-insider/grok-desktop/commit/18b3c0a24c5e262afc539013939b6d79e1296cc0))
* **desktop:** install full shadcn component catalog ([d0e9533](https://github.com/grok-insider/grok-desktop/commit/d0e9533a1683eeeeaf56ba8fc37670942eec5063))
* **desktop:** move remaining selects and tablists to shadcn ([90bf0fa](https://github.com/grok-insider/grok-desktop/commit/90bf0fa386763b25ac6979230e62ad8096803643))
* **desktop:** move the shell to the shadcn sidebar ([e2b6fd8](https://github.com/grok-insider/grok-desktop/commit/e2b6fd8d146d4946a6d0fb04ac833963b462183b))
* **desktop:** render safe rich chat responses ([6429c3a](https://github.com/grok-insider/grok-desktop/commit/6429c3a41a9490332a6fb14aa4720fddcfed07fe))
* **desktop:** scaffold package, Vite, and design system ([79aad11](https://github.com/grok-insider/grok-desktop/commit/79aad119d81d7b46fb98b9f8f330ec870e061dc8))
* **desktop:** show product labels for chat models ([9e7ffa8](https://github.com/grok-insider/grok-desktop/commit/9e7ffa82ee8d7d4d6e3695acc31728dbd063eedd))
* **desktop:** surface usage summary in settings and turns ([e711516](https://github.com/grok-insider/grok-desktop/commit/e711516454a206fb926abad90220e5e70f0d6eca))
* **domain:** add conversation, run, and effect models ([d12fe40](https://github.com/grok-insider/grok-desktop/commit/d12fe404a587f68c0e2f88f681f54a1f2732b6ba))
* **domain:** add core IDs and library root ([bf7be26](https://github.com/grok-insider/grok-desktop/commit/bf7be26151d59040209d97ac1f2cca56d8dac8e2))
* **domain:** add workspace, approval, capability, and automation ([67f70cb](https://github.com/grok-insider/grok-desktop/commit/67f70cb277ec30dda473d1b423682c376e484bce))
* **guest:** add integration runner core ([9f0d913](https://github.com/grok-insider/grok-desktop/commit/9f0d913e96b3c83f60672435759bc64e47b1d94f))
* **guest:** add NixOS guest image config ([7afbe8e](https://github.com/grok-insider/grok-desktop/commit/7afbe8ee65f520efff5ba2c34441c34281f09aae))
* **guest:** add workspace mounter and protocol ([22f6d3f](https://github.com/grok-insider/grok-desktop/commit/22f6d3fc3b883545df2d46303d6569804d4d3b33))
* **integrations:** add first-party Wisp adapter ([c83cd78](https://github.com/grok-insider/grok-desktop/commit/c83cd784a1e5ef585eded79456f83cdf77df3ebd))
* **integrations:** add schema contracts ([4fcf45e](https://github.com/grok-insider/grok-desktop/commit/4fcf45e26ac71e7efc0e0faa16d5f0eca854ca18))
* **linux:** add package:linux entry and QEMU/KVM broker contract ([531b662](https://github.com/grok-insider/grok-desktop/commit/531b66242e6b3433d895715b9462da32f4d47d8c))
* **linux:** add vm-service unix socket server and daemon dialer ([70838a4](https://github.com/grok-insider/grok-desktop/commit/70838a4894bdfc11eb574c79fd6977a60d3f9914))
* **linux:** broker gateway, Grok Build host auth, and capability facts ([7cb95a6](https://github.com/grok-insider/grok-desktop/commit/7cb95a691533bdf24277bdfba6c5ad37aad57dea))
* **linux:** harden signed broker and guest boundaries ([b8210b0](https://github.com/grok-insider/grok-desktop/commit/b8210b0699fdced9260a012465f77c38b7e685c0))
* **linux:** product isolation path EnsureImage→StartVm→health ([97c1b13](https://github.com/grok-insider/grok-desktop/commit/97c1b134486cef6c4f4a72ea7fff0c07b74244c0))
* **memory:** add conversation memory adapter ([fd1a218](https://github.com/grok-insider/grok-desktop/commit/fd1a2187d4ac3d32722041bd97c1db3d6ad3c4e0))
* **native:** add HCS service, tenant storage, and cmd ([b3697b2](https://github.com/grok-insider/grok-desktop/commit/b3697b26472bbafa8f3c2ff58a14bc21d901d470))
* **native:** add host frame server and guest channel ([2ce250b](https://github.com/grok-insider/grok-desktop/commit/2ce250bc51455dec85d7652d042d04b4d86069b0))
* **native:** add transport and desktop client policy ([353418f](https://github.com/grok-insider/grok-desktop/commit/353418f89012e20ae061582a68b487bc4bc0c56b))
* **native:** scaffold Windows VM service module ([4d2b0d4](https://github.com/grok-insider/grok-desktop/commit/4d2b0d4e4a9a469e651329028df57665f3c27801))
* **oauth:** add daemon-owned enrollment and token rotation ([d9ee100](https://github.com/grok-insider/grok-desktop/commit/d9ee10064d0b7aa7987ecd9e86135657689a3c3c))
* **oauth:** add fixed-origin supergrok authorization client ([5819ecf](https://github.com/grok-insider/grok-desktop/commit/5819ecfcbe37eafcdbe844e4ba8168a392983c36))
* **proto:** add daemon IPC protobuf ([2133bfe](https://github.com/grok-insider/grok-desktop/commit/2133bfe211af59923eabfa78c3d8316073bad188))
* **proto:** add guest channel protobuf ([a1cde9f](https://github.com/grok-insider/grok-desktop/commit/a1cde9f180c02dd0f1a9e6e8386a0e00e28aa836))
* **protocol:** add host work enrollment contracts ([4009b2a](https://github.com/grok-insider/grok-desktop/commit/4009b2a8e9eda7bd7eb8ee69c704389deb36e663))
* **protocol:** add IPC DTOs, validation, and build ([f4e4db9](https://github.com/grok-insider/grok-desktop/commit/f4e4db9fec6aa776cca88dc9c21a1bd9cff5f9fd))
* **protocol:** epoch-23 local usage summary IPC ([b80610c](https://github.com/grok-insider/grok-desktop/commit/b80610c70b2d6fc2d658601c57f2cb3f3c4f545c))
* **release:** add signed update manifest contract ([b395677](https://github.com/grok-insider/grok-desktop/commit/b39567739023246d08d1967bd84f36d4d4b4b729))
* **release:** assemble Windows core package ([f3fd924](https://github.com/grok-insider/grok-desktop/commit/f3fd9248cdaedbfb5a1e42b04f13e2b2ff7f7001))
* **release:** automate protected release preparation ([c6e520f](https://github.com/grok-insider/grok-desktop/commit/c6e520fd35a4598ceb4dc6836ebd7d98cd32582f))
* **release:** build updateable linux appimages ([d400410](https://github.com/grok-insider/grok-desktop/commit/d4004108b2b91f5d6d4f53e3bcf11cfd090af502))
* **release:** emit automatic msix update metadata ([1383642](https://github.com/grok-insider/grok-desktop/commit/138364290c41f0f07286355b83b766f262393b16))
* **release:** pin official Grok Build component ([73d6987](https://github.com/grok-insider/grok-desktop/commit/73d6987de7c0c368245ca25a696c71eeb5994d20))
* **release:** prepare v0.0.1 preview distribution ([f688421](https://github.com/grok-insider/grok-desktop/commit/f688421e637c7e01e815fb4432b37bcb327ac6e9))
* **release:** prepare v0.0.1 preview distribution ([e82192c](https://github.com/grok-insider/grok-desktop/commit/e82192cdb2fe34f8313c52af30ba95d1c390a22b))
* **scheduler:** arm epoch-18 schedule_active and durable execute_due ([a71d7ef](https://github.com/grok-insider/grok-desktop/commit/a71d7ef4757ce51da9a03b2719614786dd0f8907))
* **settings:** surface active supergrok api rail ([5183776](https://github.com/grok-insider/grok-desktop/commit/5183776cb4bc6f6c67998bbe65558379d4312a06))
* **sqlcipher:** add conversation and credential stores ([9293139](https://github.com/grok-insider/grok-desktop/commit/9293139242f1d66e49c337dacb733d81325eaae7))
* **sqlcipher:** add schema and core store ([789ade0](https://github.com/grok-insider/grok-desktop/commit/789ade08c645ac27fe4c77458ea011a7a84e6fee))
* **sqlcipher:** add workspace, preferences, and artifact stores ([bab9a2c](https://github.com/grok-insider/grok-desktop/commit/bab9a2c66b072254de2849206e70b37c04493549))
* **sqlcipher:** journal host enrollment mutations ([e4d0f39](https://github.com/grok-insider/grok-desktop/commit/e4d0f393e97ae92ee3b90997b498c02289d4e0e5))
* **sqlcipher:** persist host policy and work backend ([f36ec70](https://github.com/grok-insider/grok-desktop/commit/f36ec709ccaaf3aa2b9400fdc74e5691f1cd7b23))
* **update:** add signed beta release channel ([d606c88](https://github.com/grok-insider/grok-desktop/commit/d606c8874f64eaf190ab0190bf66eba554f576b6))
* **updater:** add bounded msix update coordinator ([15345d7](https://github.com/grok-insider/grok-desktop/commit/15345d7d5bd842ddafd0102d791149c032f17f71))
* **updater:** add verified linux appimage self-update ([38e1e0a](https://github.com/grok-insider/grok-desktop/commit/38e1e0a729da7d586407269407fe883c35bddab9))
* **updater:** expose trusted main-process update commands ([ed3213a](https://github.com/grok-insider/grok-desktop/commit/ed3213a23ba3c2277c9c4ae329f0b0a0e385dbc6))
* **updater:** require signed release authorization ([7d8ad6e](https://github.com/grok-insider/grok-desktop/commit/7d8ad6e759d4663efbeee4042122e3fddc73f8f7))
* **usage:** aggregate completed-turn usage by scope and window ([ac8c92b](https://github.com/grok-insider/grok-desktop/commit/ac8c92bd21ceba1f1ccb9288c9b2b32d162ad770))
* **vault:** add OS keyring vault adapter ([d26e149](https://github.com/grok-insider/grok-desktop/commit/d26e14983ecd8c3fd3539aa72c02000dda83fa5a))
* **vm-service-client:** add fail-closed VM broker client ([12fe5ab](https://github.com/grok-insider/grok-desktop/commit/12fe5abef7a871a604a8bff54d1d30b1bf85bb23))
* **windows-acl:** add audited Win32 ACL boundary crate ([e888037](https://github.com/grok-insider/grok-desktop/commit/e88803733c1001d673d609eeaae4980737e25343))
* **wisp:** signed install lifecycle with epoch-19 IPC ([69ffe8d](https://github.com/grok-insider/grok-desktop/commit/69ffe8dd16bb7d77d6319a0a3ad881e0c71909ad))
* **work:** add capability-rooted host filesystem reads ([2390a74](https://github.com/grok-insider/grok-desktop/commit/2390a74bc84ea3fac7df892ef10f9d6a509d2fc2))
* **work:** add policy-free host tools MCP helper ([47b97c0](https://github.com/grok-insider/grok-desktop/commit/47b97c0e8e3fd9c68eb7f3afe57cd3e595103c36))
* **work:** authenticate per-run host tool bridge ([36610ff](https://github.com/grok-insider/grok-desktop/commit/36610ff51bf7ba756c9710849afd94bd4e0116ce))
* **work:** dispatch durable host work turns ([66f641c](https://github.com/grok-insider/grok-desktop/commit/66f641ca56b080b7785c6e49f299e280593d3517))
* **work:** journal approved host mutations ([5a232ef](https://github.com/grok-insider/grok-desktop/commit/5a232ef2178062762d7116c58dad04db57f4c789))
* **work:** secure and package host tools bridge ([202663c](https://github.com/grok-insider/grok-desktop/commit/202663c2aba1045dae944b12d9175351e44b0d8a))
* **work:** support multi-turn host conversations ([b07a7c5](https://github.com/grok-insider/grok-desktop/commit/b07a7c528468393e01ed20491e4b1a390618e740))
* **xai:** add official xAI API client adapter ([161d752](https://github.com/grok-insider/grok-desktop/commit/161d75273f1ffa303a343f8d0c6538ef5c331178))


### Bug Fixes

* **acp:** accept official runtime skills on restart ([3f01178](https://github.com/grok-insider/grok-desktop/commit/3f011785be4193f5a502c577503189bced129848))
* **acp:** preserve credential and filesystem boundaries ([bb8c5ed](https://github.com/grok-insider/grok-desktop/commit/bb8c5edb80f82db76797cb69070478f522f5b535))
* **chat:** bind complete provider request fingerprint ([226c282](https://github.com/grok-insider/grok-desktop/commit/226c282a144f7415fac38439c5826438693e5b19))
* **chat:** enable supergrok-only capability readiness ([090d33e](https://github.com/grok-insider/grok-desktop/commit/090d33e167d4dccd496defed6323b522d38d16b2))
* **chat:** exclude Imagine media models from text chat readiness ([1e4d08a](https://github.com/grok-insider/grok-desktop/commit/1e4d08ab0fdd3be51ac54568bf0b64aa12c90426))
* **chat:** omit empty modelId on conversation start ([5a5f5e9](https://github.com/grok-insider/grok-desktop/commit/5a5f5e9e23dc4702ae1080e4e6c6f66aff1ea22d))
* **ci:** install pnpm before restoring cache ([2b8fb36](https://github.com/grok-insider/grok-desktop/commit/2b8fb36eac965c311b97013cf32a08caee3f08fb))
* **ci:** satisfy duration lint in broker smoke test ([53d52b7](https://github.com/grok-insider/grok-desktop/commit/53d52b733372f69a15e058c2b8cf50adc54f8541))
* **ci:** use supported npm audit endpoint ([3af0c21](https://github.com/grok-insider/grok-desktop/commit/3af0c21af2ac416972e5f1d80e875eb95d31e34a))
* **desktop:** accept modelId on conversation start bridge ([ad04e3a](https://github.com/grok-insider/grok-desktop/commit/ad04e3a31466c8f84ff6d716dfaf14e17330d1cc))
* **desktop:** adapt Linux graphics backend at startup ([770fdd7](https://github.com/grok-insider/grok-desktop/commit/770fdd707f0001a2b72f7864b3ec53ed1d85a44e))
* **desktop:** advertise only daemon-backed Settings surfaces ([6f4342a](https://github.com/grok-insider/grok-desktop/commit/6f4342a162219419ad6f99122cd1859b7396afe7))
* **desktop:** de-advertise Library Imagine media creation surfaces ([ddaaf51](https://github.com/grok-insider/grok-desktop/commit/ddaaf5161b76d064d8e21ad83f1952f0535b4bf7))
* **desktop:** gate model discovery on chat readiness ([dbad72c](https://github.com/grok-insider/grok-desktop/commit/dbad72c22e7c92027c8ce5019ac4bbba9c1debe4))
* **desktop:** keep capability projections protocol accurate ([41bccf6](https://github.com/grok-insider/grok-desktop/commit/41bccf697941fb940c842b71587ac893ee50bd4d))
* **desktop:** label supergrok api rail explicitly ([770cef2](https://github.com/grok-insider/grok-desktop/commit/770cef27236763dc3c5dfb350be4a00e978dfa9e))
* **desktop:** map usageSummary in daemon RPC ResultValueMap ([065c4ba](https://github.com/grok-insider/grok-desktop/commit/065c4ba792d1deea119f6444c87264d393d50b31))
* **desktop:** point Settings subscription row at Setup host auth ([b2f02a1](https://github.com/grok-insider/grok-desktop/commit/b2f02a13a948dfed392ffef7fb99b9a52e7391e8))
* **desktop:** remove Wisp install from product advertising ([2c20128](https://github.com/grok-insider/grok-desktop/commit/2c2012888514351db53fc5ba0b5b62f425e61fad))
* **desktop:** settle native viewport before post-emulation probes ([de74f3b](https://github.com/grok-insider/grok-desktop/commit/de74f3b7f9c5f0a8a08c32449b70d13539bd9405))
* **desktop:** stabilize Nvidia Nix software rendering ([ec8999e](https://github.com/grok-insider/grok-desktop/commit/ec8999e820172ec24423dd6bc69bf214fe3c994a))
* **desktop:** unclip model menu, hide menu bar, show app version ([55eefc6](https://github.com/grok-insider/grok-desktop/commit/55eefc62c52d803f61ccb647245751dba22b4a62))
* **desktop:** use opaque focus rings in generated primitives ([30cd01a](https://github.com/grok-insider/grok-desktop/commit/30cd01a7e0d77bd12f1301100bc2b92916585611))
* **dev:** isolate daemon state by launch profile ([2f249f1](https://github.com/grok-insider/grok-desktop/commit/2f249f12956c68bc762eac8fc59f2d7e032b97cf))
* **integrations:** complete catalog example envelope ([7409312](https://github.com/grok-insider/grok-desktop/commit/740931274a9d16fabce4d1aa419f718defb73b59))
* **linux:** decode guest_control body as Go base64 wire ([38b53b8](https://github.com/grok-insider/grok-desktop/commit/38b53b892639101f30d8676a5d5691a74f7c806f))
* **linux:** epoch-17 tests, hypervisor spawn gates, isolation gateway wiring ([5d3c316](https://github.com/grok-insider/grok-desktop/commit/5d3c3162d9c8955286a915d7625a87619e5fbe59))
* **nix:** refresh guest runner module hash ([e082917](https://github.com/grok-insider/grok-desktop/commit/e08291733dff7723719a64aace821e8b337729a5))
* **protocol:** fail closed unfinished execution paths ([e5d7af4](https://github.com/grok-insider/grok-desktop/commit/e5d7af4897fc82a24084490a5b303e690a32daf1))
* **quality:** detect go package-ok lines in wisp lifecycle evidence ([66787d5](https://github.com/grok-insider/grok-desktop/commit/66787d5ef3fe4bd23aadb49951b8d347ab32e8b6))
* **quality:** reconcile usage baseline and documentation ([454b642](https://github.com/grok-insider/grok-desktop/commit/454b642b079e9bac22d8cb912e7547ba9f576fb3))
* **release:** bind preview component evidence ([762a2cf](https://github.com/grok-insider/grok-desktop/commit/762a2cf21a7b84ecb7919876214cc0d931481cf1))
* **release:** expose qualified MSVC discovery ([b99957b](https://github.com/grok-insider/grok-desktop/commit/b99957b511c98146bddba44daac99f451cdff013))
* **release:** finalize automated release tags ([8925409](https://github.com/grok-insider/grok-desktop/commit/8925409243e644c7b3328b4518fc8716eac45e96))
* **release:** isolate prerequisite signing secrets ([5c8840c](https://github.com/grok-insider/grok-desktop/commit/5c8840cb5f5dd0a4817fdba8bd8a15cb26e37ed4))
* **release:** qualify Windows MSVC discovery ([e73dc8a](https://github.com/grok-insider/grok-desktop/commit/e73dc8aa32db1c065b5138099c200dbd7bd1a191))
* **release:** unblock v0.0.1 qualification ([a65a259](https://github.com/grok-insider/grok-desktop/commit/a65a2591f8d0247379cce0eb7298cbaebb213287))
* **release:** unblock v0.0.1 qualification ([f186620](https://github.com/grok-insider/grok-desktop/commit/f1866200b3500b233f0fd7ea9b66185cc27fff80))
* **release:** validate qualified Windows inputs ([83a06f7](https://github.com/grok-insider/grok-desktop/commit/83a06f795081bed28a9174cdfeb7fc2c0263399b))
* **security:** enforce frame policy in protocol headers ([94d2f17](https://github.com/grok-insider/grok-desktop/commit/94d2f1787f5b22b56a7b33e4dcd75ad45b61bd5e))
* **settings:** open the selected api setup rail ([893d652](https://github.com/grok-insider/grok-desktop/commit/893d652c40c13d6dae09803df36432af5e59ab95))
* **storage:** harden cross-platform database runtime ([08ec17c](https://github.com/grok-insider/grok-desktop/commit/08ec17ca72bf84e7dded66f1c341641eda812159))
* **windows:** align daemon composition contracts ([71b846c](https://github.com/grok-insider/grok-desktop/commit/71b846ce3de4369893d2481eb1a411e222c16fab))
* **windows:** canonicalize managed storage paths ([fe009fe](https://github.com/grok-insider/grok-desktop/commit/fe009fe0acb118c7bb0cd5e807ba8d2a469a064b))
* **windows:** disambiguate owned ACL handles ([a30a04a](https://github.com/grok-insider/grok-desktop/commit/a30a04a163b95dc61918cccfa8ee229a619b422a))
* **windows:** finalize private file publication ([9f5645a](https://github.com/grok-insider/grok-desktop/commit/9f5645a79285cb162e2c22cc1468cd51e016ec98))
* **windows:** gate MCP process contract fixture ([2e613a1](https://github.com/grok-insider/grok-desktop/commit/2e613a16f01d2cb66eb54651f1e5d76dc5a77ee3))
* **windows:** gate Unix host tool fixtures ([be3dcdc](https://github.com/grok-insider/grok-desktop/commit/be3dcdc30ee69acf98f415d380c7757527a1e9e4))
* **windows:** permit atomic managed file publication ([c8268a9](https://github.com/grok-insider/grok-desktop/commit/c8268a97408545be471924b13d5d5a20bb33c87f))
* **windows:** preserve SQLCipher platform contracts ([87dbd1a](https://github.com/grok-insider/grok-desktop/commit/87dbd1a16ceb4ebbb0717d6cfcfd40f91f138187))
* **windows:** publish managed files by handle ([cf501b5](https://github.com/grok-insider/grok-desktop/commit/cf501b533c485cdcecda85afc639ecee691aa365))
* **windows:** satisfy ACP platform lint contract ([feedc4f](https://github.com/grok-insider/grok-desktop/commit/feedc4f5a7047a3fc4758d9ddbedb9ca02e8c137))
* **windows:** scope daemon platform helpers ([ea125a7](https://github.com/grok-insider/grok-desktop/commit/ea125a7bf267d2e065bf2dca7fd3ebf327c6c261))
* **work:** close host authority state gaps ([d268147](https://github.com/grok-insider/grok-desktop/commit/d2681476adb01ba3700ce152d4a89c7291e47853))
* **work:** complete daemon host tool turns ([4fcbdb9](https://github.com/grok-insider/grok-desktop/commit/4fcbdb923005b2929ea2e77729b1e5bc1b84533d))
* **work:** prepare host tools from development launches ([398c05a](https://github.com/grok-insider/grok-desktop/commit/398c05ab0dbdd70a6f2c91d436977d2d6cdc3dfb))
* **work:** restore official ACP runtime startup ([1342af3](https://github.com/grok-insider/grok-desktop/commit/1342af3aaa6d7bb0c514d5467c6f10788cb99af2))


### Performance Improvements

* **ci:** cache Rust build artifacts ([9012263](https://github.com/grok-insider/grok-desktop/commit/90122634db5868cd6a71d51f27e0958baef65726))
