# Changelog

## [0.3.0](https://github.com/hyperterse/seaport/compare/v0.2.0...v0.3.0) (2026-06-09)

### Features

* add upgrade command ([238a9ab](https://github.com/hyperterse/seaport/commit/238a9abd5ea76e36f3ec641dc1ed2be226ad1ef0))

## 0.2.0 (2026-06-09)

### Features

* add deterministic evaluation core ([e5f1b47](https://github.com/hyperterse/seaport/commit/e5f1b47a75f94e3647ac81dcbb40ffa76b3915cd))
* add direct nop agent execution ([e158cfe](https://github.com/hyperterse/seaport/commit/e158cfeb918325d40210e365bd887a49db1eaf10))
* add sandboxed docker execution backend ([530513a](https://github.com/hyperterse/seaport/commit/530513a0e9ee3247ce24c3e18a2dcefd7750c016))
* add seaport CLI and project naming ([e5bc6c9](https://github.com/hyperterse/seaport/commit/e5bc6c95bcd640a0670bcfd10db836f7a511c4c7))
* align task environment parsing with harbor ([658a188](https://github.com/hyperterse/seaport/commit/658a188e05630efb20fcb0b6a5ff792ef9bb1150))
* group resolution progress under run header ([bf94c57](https://github.com/hyperterse/seaport/commit/bf94c573f86fac97b2892a9ad8b56230123ae50c))
* improve run logging ([80444fe](https://github.com/hyperterse/seaport/commit/80444fee73b11b6a17618a3c0bf9c257b5658ca2))
* make run output terminal friendly ([8b2ecaf](https://github.com/hyperterse/seaport/commit/8b2ecaff81584d0695972ffa2c0dfc3545ae30ce))
* pass phase environment variables ([f272892](https://github.com/hyperterse/seaport/commit/f27289255b910333e055a5099bc6a93bff5db8b2))
* resolve git-backed registry tasks ([46b5995](https://github.com/hyperterse/seaport/commit/46b59956e66ea3bacf084d9c41494ad667504b28))
* resolve harbor local registry datasets ([a8f31c9](https://github.com/hyperterse/seaport/commit/a8f31c90fbd4662fe451e89b71f17a6ac5f65f32))
* resolve remote package datasets ([65d0c16](https://github.com/hyperterse/seaport/commit/65d0c16a535491743194fa2a0c987bbf0f48de0f))
* run direct registry and git tasks ([76904ae](https://github.com/hyperterse/seaport/commit/76904aec2bba95f1ab0def74f2c7360b937ff58c))
* run harbor-compatible local datasets ([6d8b7de](https://github.com/hyperterse/seaport/commit/6d8b7dee894638b75fc21fac4005ff9c8eef6f07))
* run local oracle tasks ([8a6fe5d](https://github.com/hyperterse/seaport/commit/8a6fe5d8b56589121db86e1f94337b87cc18578a))
* run sandboxed external agent commands ([48e018b](https://github.com/hyperterse/seaport/commit/48e018b9c7c6b1008b896f46c1c799d29cfb3bac))
* run task attempts concurrently ([94a5e40](https://github.com/hyperterse/seaport/commit/94a5e40111e71976bb156c6c5a31680e9df56d8e))
* show run startup progress ([d7c4949](https://github.com/hyperterse/seaport/commit/d7c494940e5cc572c92e759360d329e14e439bf9))
* show task durations in run output ([f9aa331](https://github.com/hyperterse/seaport/commit/f9aa33144daa55807e9505151905e96751319a05))
* stream task execution logs ([b07261f](https://github.com/hyperterse/seaport/commit/b07261f6f8e31537683616f15daa259c2d5dac3b))

### Bug Fixes

* align docker runtime with benchmark toolchains ([904ed2f](https://github.com/hyperterse/seaport/commit/904ed2f4458533c3e6c67cb3eba44e62ba084296))
* allow sandboxed tmp binaries to execute ([1f8a034](https://github.com/hyperterse/seaport/commit/1f8a0343aa08f242a1121ae99aba3a03ce68c033))
* build task dockerfiles on native platform ([1d09989](https://github.com/hyperterse/seaport/commit/1d09989ecf85362046d30ea8b537776270d58360))
* expose cobol copybooks at workspace root ([a86f426](https://github.com/hyperterse/seaport/commit/a86f426b036be6fd913af0afd9b5b4e9eaaca1f9))
* gate docker api response parsing to unix ([411c805](https://github.com/hyperterse/seaport/commit/411c805d81ebf50d156960e98efa5ac253381c6e))
* honor task docker resource limits ([e84f8a0](https://github.com/hyperterse/seaport/commit/e84f8a07f234cc379fcfdb87d8ddb98219cd5200))
* infer amd64 platform for legacy Java images ([a4af2ce](https://github.com/hyperterse/seaport/commit/a4af2ce0c6c3e09f90e7364e160f10acb70f44da))
* infer amd64 platform for x86 assembly tasks ([8402b98](https://github.com/hyperterse/seaport/commit/8402b98c50a5bc9d7e2b1a29bc50431aa73617bb))
* match benchmark container layout ([62e5385](https://github.com/hyperterse/seaport/commit/62e5385ce92614fd7d287ea7f19d814444995a5a))
* prefer native docker platform when available ([903dabf](https://github.com/hyperterse/seaport/commit/903dabf545b22ef5feff06366cea158f8850d2fc))
* preserve docker image workspace ([7b0867d](https://github.com/hyperterse/seaport/commit/7b0867d3a78e2e8935dd0aa6949e1279e09d522d))
* record failed trials without stopping runs ([3333674](https://github.com/hyperterse/seaport/commit/3333674453ac29501db3603b5cddf1ab97ff371a))
* report each task's share of the execution timeline ([0cbeb07](https://github.com/hyperterse/seaport/commit/0cbeb07ff4871b21a7abbf9e79e25d152bccfc7f))
* retry package archive downloads ([ed9efbd](https://github.com/hyperterse/seaport/commit/ed9efbdfe8b3a4f904fd2d5b8b325f4ca4129fee))
* retry transient docker build failures ([52f24b9](https://github.com/hyperterse/seaport/commit/52f24b93ab186b85cbbce33b112595cc81922919))
* satisfy formatting and clippy checks ([e9925c5](https://github.com/hyperterse/seaport/commit/e9925c5f5d1d3cbbce28f2aa9c374538ba162b3b))
* set docker platform for task containers ([1235408](https://github.com/hyperterse/seaport/commit/1235408b18ee34ad2d1b6c78ebe8cb3d1d20e4fa))
* stage task files in execution workspaces ([6c0f46e](https://github.com/hyperterse/seaport/commit/6c0f46e7a4a4339ce59905eb1c918164f698186e))
* stream docker build progress ([c8604f9](https://github.com/hyperterse/seaport/commit/c8604f9ff93cd21d731e81d723b0b68f752ee834))
* use buildkit-compatible network mode ([ac36a85](https://github.com/hyperterse/seaport/commit/ac36a853de71e52a2e2268226735a99582463cd1))

### Performance Improvements

* adapt default run concurrency ([f209a1b](https://github.com/hyperterse/seaport/commit/f209a1b070c17ff14f6be8a1e405429fb0f9c695))
* cache docker task environments ([acec736](https://github.com/hyperterse/seaport/commit/acec73602db1e00f49643a7991025d995e6b327e))
* deduplicate docker image pulls ([10bfeaf](https://github.com/hyperterse/seaport/commit/10bfeaf42eb2c1d6b2b54880489101ba5eae09c9))
* preflight docker environments ([fdf0f00](https://github.com/hyperterse/seaport/commit/fdf0f00eed0a61092028e6e1d73c777a8c9366a1))
* raise adaptive concurrency cap ([9631112](https://github.com/hyperterse/seaport/commit/96311126a84f74abc150c517cfdf2e56a11031b8))
* schedule long-running trials first ([e1196c1](https://github.com/hyperterse/seaport/commit/e1196c1eed2ac40cfabec8f0cf0722e8c5c3d508))
* schedule work by run phase ([db643a1](https://github.com/hyperterse/seaport/commit/db643a1d3ae3d0d406d305666663ff15b9396d6d))
* speed up workspace snapshot restores ([f3195fb](https://github.com/hyperterse/seaport/commit/f3195fb6d6397a6a12c7d8be243899434bba56ba))
* use a managed buildkit builder ([358f7cd](https://github.com/hyperterse/seaport/commit/358f7cda706081b9dfc7e9ea3553ff5dc3803f56))
* use docker api for control-plane checks ([8b5cfea](https://github.com/hyperterse/seaport/commit/8b5cfea1994e9c2f9f0e7348d98623754044f10e))
