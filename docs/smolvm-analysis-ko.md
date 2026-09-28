# smolvm 전수조사 분석 & 수익화 리포트 (한국어)

> 작성: Claude Code (카리나 페르소나) · 요청자: bmshin94
> 작성일: 2026-09-28
> 대상 커밋 시점 버전: `smolvm v1.16.1`

## 📎 관련 GitHub / 링크 모음

| 대상 | 주소 |
|---|---|
| **이 저장소 (오빠 포크)** | https://github.com/bmshin94/smolvm |
| **업스트림 원본** | https://github.com/smol-machines/smolvm |
| 공식 문서 사이트 | https://smolmachines.com/docs |
| 문서 저장소 | https://github.com/smol-machines/docs |
| 릴리스 다운로드 | https://github.com/smol-machines/smolvm/releases |
| 별도 SDK (외부 프로세스 방식) | https://github.com/smol-machines/smolvm-sdk |
| VMM: libkrun | https://github.com/containers/libkrun |
| 게스트 커널: libkrunfw | https://github.com/smol-machines/libkrunfw |
| 제작자 | https://github.com/BinSquare · https://x.com/binsquares |
| Discord | https://discord.gg/E5r8rEWY9J |
| GPU 원격화 설계 글 | https://smolmachines.com/engineering/gpu-over-vsock |

---

## 1. 정체 요약

**smolvm = OCI-native microVM runtime.**
도커 이미지를 *컨테이너*가 아니라 **독립 커널을 가진 진짜 VM**으로 **200ms 미만**에 부팅하는 CLI 도구.

| 항목 | 내용 |
|---|---|
| 언어 | Rust (233개 `.rs` 파일 / 약 21만 줄) |
| 버전 | `v1.16.1` |
| 라이선스 | Apache-2.0 (상업적 이용 가능) |
| 아키텍처 | 데몬 없음. libkrun을 라이브러리로 링크 |
| 하이퍼바이저 | Hypervisor.framework(macOS) / KVM(Linux) / WHP(Windows) |
| 제작 | @binsquare / smol-machines |

### 이 저장소의 정체
업스트림 `smol-machines/smolvm`의 **포크**이며, 이 포크에 추가된 커밋은 다음 하나뿐이다.

```
238c877  docs: created CLAUDE.md persona guide   (CLAUDE.md, +25줄)
612ed15  Merge pull request #1 from bmshin94/feat/claude-guide
```

즉 나머지 전부는 업스트림 원본 코드다. 분석/학습 목적의 포크로 보는 것이 정확하다.

---

## 2. 폴더 전수조사

```
smolvm/
├── src/              본체 (CLI + 런타임 + HTTP API) — 4.4MB
│   ├── cli/          machine / pack / serve 명령 구현
│   ├── vm/           VM 상태머신, libkrun 백엔드, Rosetta(x86 on ARM)
│   ├── agent/        게스트 에이전트: exec, 파일전송, VNC, 비디오, fork,
│   │                 virtiofs, 입력장치, vsock 서비스  ← 핵심
│   ├── network/      네트워크 백엔드 + egress 정책
│   ├── api/          REST API, 풀 컨트롤러, rollout(RL), 승인제어, 디바이스 핸드오프
│   ├── embedded/     라이브러리 임베드 모드 (데몬 없이 in-process)
│   ├── checkpoint_store.rs / portable_checkpoint.rs   VM 상태 저장·복원
│   ├── image_store.rs / registry.rs                  OCI 이미지 pull/push
│   ├── secrets.rs                                    시크릿을 "참조"로만 취급
│   └── dns_filter*.rs                                DNS 레벨 차단
│
├── crates/           15개 서브 크레이트 — 4.3MB
│   ├── smolvm-agent        게스트 init/에이전트
│   ├── smolvm-cuda / -cuda-guest / -cuda-shim / -cuda-codegen
│   │   -cudart-shim / -nvml-shim        CUDA API 원격화 계열
│   ├── smolvm-network      네트워킹 스택
│   ├── smolvm-oci-layer    OCI 레이어 처리
│   ├── smolvm-pack         .smolmachine 패킹 + 서명 + Mach-O 조작
│   ├── smolvm-protocol     호스트↔게스트 프로토콜
│   ├── smolvm-registry     레지스트리 클라이언트
│   ├── smolvm-s3fs         S3 → 파일시스템
│   ├── smolvm-shim         containerd shim v2 (쿠버네티스 연동)
│   └── smolvm-smolfile     Smolfile(TOML) 파서
│
├── sdks/
│   ├── node/     TypeScript 임베디드 SDK (napi 네이티브 바인딩)
│   │             ※ README에 "임베디드 SDK로 만든 머신은 DB를 거치지 않아
│   │               CLI에 안 보임 — 알려진 버그, 수정 중" 명시
│   └── python/   RL 학습용 rollout 클라이언트 (vLLM + LoRA 어댑터)
│
├── examples/     python-app · node-app · docker-in-vm · local-llm(llama.cpp+Vulkan)
│                 headless-browser(fork로 브라우저 풀) · gpu-chrome · doom-web
│                 desktop(Hyprland+VNC) · openclaw-app(LLM 게이트웨이 격리)
├── demo/         H100 QLoRA/DPO 벤치마크 + QA-LOG/PERF-LOG/REPRODUCIBILITY
├── deploy/       k3s 설치 스크립트, 쿠버네티스 RuntimeClass 매니페스트
├── packaging/    deb / arch(PKGBUILD) / nix / yum 배포 패키징
├── lib/          libkrun, libkrunfw, MoltenVK, virglrenderer 등 번들 네이티브 라이브러리
├── docs/         DEVELOPMENT / GPU / 증분 체크포인트 / 배포판별 설치 가이드
├── .github/workflows/  ci · release · critest(CRI 적합성) · publish-crates · nix bump
├── AGENTS.md     615줄. AI 에이전트용 완전 CLI 레퍼런스
└── CLAUDE.md     이 포크에서 추가한 페르소나 가이드
```

---

## 3. 동작 원리

```
[호스트: macOS / Linux / Windows]
   ├─ Hypervisor.framework | KVM | WHP     ← 하드웨어 가상화
   ├─ libkrun (VMM, 라이브러리로 링크 / 데몬 없음)
   ├─ libkrunfw (게스트 리눅스 커널 내장)
   └─ 워크로드마다 독립 VM + 독립 커널
```

- 도커: 커널 **공유** → 탈출 리스크가 커널 경계
- smolvm: 커널 **분리** → 탈출 리스크가 하드웨어 경계, 그런데 부팅은 200ms 미만
- 메모리는 virtio balloon으로 **탄력적** — 실제 사용량만 호스트가 커밋, 유휴분 자동 회수
- vCPU 스레드는 유휴 시 하이퍼바이저에서 sleep → 오버프로비저닝 비용 거의 0

---

## 4. 핵심 기능 7가지

### ① Smolfile — VM 전체를 TOML 한 장으로

```toml
image = "python:3.12-alpine"
net = true
cpus = 4
memory = 4096
ports = ["8000:8000", "5173-5180:5173-5180"]
volumes = ["./src:/app"]
init = ["pip install -r /app/requirements.txt"]

[network]
allow_hosts = ["api.stripe.com", "pypi.org"]

[auth]
ssh_agent = true
```

알 수 없는 키는 **무시하지 않고 거부** → 오타가 create 시점에 실패한다.

### ② 네트워크 기본 차단 (deny by default)

```bash
smolvm machine run --image alpine -- nslookup example.com            # 실패 (net off)
smolvm machine run --net --allow-host registry.npmjs.org --image alpine \
  -- wget -q -O /dev/null https://registry.npmjs.org                 # 성공
smolvm machine run --net --allow-host registry.npmjs.org --image alpine \
  -- wget -q -O /dev/null https://google.com                         # 차단
```

`--allow-host`는 DNS 필터까지 켜서 **허용 호스트만 resolve** 된다.

### ③ pack — VM을 단일 실행파일로

```bash
smolvm pack create --image python:3.12-alpine -o ./python312
./python312 run -- python3 --version        # venv/conda/pyenv 불필요

smolvm pack create --from-vm myvm -o myvm   # 손으로 세팅한 VM을 그대로 스냅샷
smolvm pack push --file myvm.smolmachine ghcr.io/you/myvm:v1
smolvm pack pull ghcr.io/you/myvm:v1
```

산출물 2개: 스텁 바이너리(플랫폼별) + `.smolmachine` 페이로드(크로스플랫폼).

### ④ branch — 실행 중 VM의 CoW 라이브 포크

```bash
smolvm machine start  --name source --branchable
smolvm machine branch --from source --count 8 --name-prefix worker --parallel 8
```

- 메모리·실행 중 프로세스·디스크까지 그대로 복제
- 자식에 `SMOLVM_BRANCH_NAME / _INDEX / _BATCH_ID / _BATCH_SIZE` 주입
- 소스 워크로드가 `smolvm-branch-ready`로 분기점 선언, 자식은 `smolvm-worker-ready`로 준비 신호
- `--wait-worker-ready` (기본 5분), 배치 분기 `--ready-timeout` 기본 10분
- 헤드리스 브라우저 기준 콜드스타트 1~3초 → **50~130ms**

### ⑤ checkpoint — 내구성 있는 상태 아티팩트

`machine checkpoint` → `.smolcheckpoint` 로 저장, 나중에/다른 곳에서 복원. 증분 체크포인트 지원(`docs/incremental-checkpoints.md`).

### ⑥ GPU 두 가지 경로 (서로 다른 인터페이스!)

| 플래그 | 방식 | 제공 |
|---|---|---|
| `--gpu` | virtio-gpu / Venus | 게스트에 **진짜 Vulkan 디바이스**. CUDA는 제공하지 않음 |
| `--cuda` | vsock으로 CUDA 호출 원격화 | 게스트에 NVIDIA 드라이버 **불필요**. GPU 패스스루 아님 |

- 실측: llama.cpp Vulkan 백엔드, Apple M4 Max → 프롬프트 634.3 t/s, 생성 170.2 t/s (Qwen2-0.5B Q4_K_M)
- `--cuda` 지원 범위는 **Driver API(`cu*`)** 까지. `nvcc`/PyTorch가 쓰는 **Runtime API(`libcudart`)는 미지원** — `cuGetExportTable`의 비공개 내부 인터페이스 때문. 별도 `smolvm-cudart-shim` 레벨 원격화가 필요

### ⑦ 쿠버네티스 — containerd shim v2 내장

```bash
sudo ./kubernetes/install-k8s-runtime.sh && sudo systemctl restart containerd
kubectl label node <node> smolvm-runtime=true
kubectl apply -f kubernetes/runtimeclass.yaml
```

파드에 `runtimeClassName: smolvm` 한 줄이면 파드가 microVM이 된다 (Kata와 같은 통합 지점).

---

## 5. 비교표 (README 기준)

|  | smolvm | 컨테이너 | Colima | QEMU | Firecracker | Kata |
|---|---|---|---|---|---|---|
| 워크로드 경계 | VM+게스트커널 | 네임스페이스+공유커널 | 공유VM 내 네임스페이스 | VM+커널 | VM+커널 | 컨테이너당 VM |
| 부팅 | **<200ms** | ~100ms | ~수초 | 15~30초 | <125ms | ~500ms |
| 아키텍처 | 라이브러리(libkrun) | 데몬 | 데몬(VM 내) | 프로세스 | 프로세스 | 런타임 스택 |
| macOS 네이티브 | 예 | Docker VM 경유 | 예(krunkit) | 예 | 아니오 | 아니오 |
| 임베디드 SDK | 예 | 아니오 | 아니오 | 아니오 | 아니오 | 아니오 |
| 포터블 아티팩트 | `.smolmachine` | 이미지(데몬 필요) | 없음 | 없음 | 없음 | 없음 |

---

## 6. 설치 및 사용법

### 설치

```bash
# 원라이너 (macOS + Linux)
curl -sSL https://smolmachines.com/install.sh | bash && smolvm --help

# 소스 빌드
cargo build --release      # rust-toolchain.toml 로 버전 고정
cargo make                 # Makefile.toml (cargo-make)
```

배포판별 가이드: `docs/install-arch.md`, `install-debian.md`, `install-fedora.md`, `install-nix.md`
Windows: `windows-x86_64` 릴리스 압축 해제 후 `smolvm.exe` (WHP 기능 활성화 필요)

### 플랫폼 요건

| 호스트 | 게스트 | 요건 / 주의 |
|---|---|---|
| macOS Apple Silicon | arm64 Linux | macOS 11+. `com.apple.security.hypervisor` 엔타이틀먼트 서명 필수. 재빌드/재서명 시 소실되면 모든 VM start가 `krun_start_enter returned: -22 (EINVAL)` 로 실패 → `codesign --force --sign - --entitlements hv.entitlements <bin>` |
| macOS Intel | x86_64 Linux | macOS 11+ (미검증) |
| Linux x86_64 / aarch64 | 동일 아키텍처 | `/dev/kvm` 접근 권한 |
| Windows x86_64 | x86_64 Linux | WHP. GPU 불가, branch/checkpoint 불가, 네트워크는 TSI 기반 |

### 자주 쓰는 명령

```bash
# 일회성 (종료 시 전부 정리)
smolvm machine run --net --image alpine -- sh -c "echo hi && uname -a"
smolvm machine run --net -it --image alpine -- /bin/sh

# 영속 머신
smolvm machine create --net --name myvm --image ubuntu
smolvm machine start  --name myvm
smolvm machine exec   --name myvm -- apt-get install -y python3
smolvm machine shell  --name myvm
smolvm machine stop   --name myvm
smolvm machine delete --name myvm

# 상태 / 파일 / 스트리밍
smolvm machine ls [--json] · status · stats · monitor
smolvm machine cp ./script.py myvm:/workspace/script.py
smolvm machine cp myvm:/workspace/out.json ./out.json
smolvm machine exec --stream --name myvm -- python3 train.py

# 로컬 컨테이너 이미지 (CI / 에어갭)
docker save myapp | smolvm machine run --image - -- ./app
smolvm machine run --image ./myapp.tar -- ./app
smolvm machine run --image ./rootfs/   -- ./app

# HTTP API
smolvm serve start --listen 127.0.0.1:8080
smolvm serve openapi
```

### 기본값 / 영속성 모델

- 이름 생략 → `"default"`, **네트워크 OFF**, CPU 4, 메모리 8192MiB, 스토리지 20GiB, 오버레이 2GiB
- `machine run` = 휘발 / `machine exec` = 영속(오버레이에 보존) / `stop`+`start` 후에도 보존
- `/tmp`, `/run`, `/dev/shm` = tmpfs (재시작 시 초기화) → **자격증명·설정을 여기 두지 말 것**
- `/workspace` = 스토리지 디스크 기반 영속. `-v host:/workspace` 를 주면 호스트 마운트가 우선
- 파일 전송: 1MiB 이하 단일 메시지, 초과분 자동 청크(업 1MiB / 다운 16MiB), 전송당 최대 4GiB
  macOS(Apple Silicon) 실측 ~35–42MB/s 업로드, ~170MB/s 다운로드

---

## 7. 플러그인? 스킬? MCP?

**전부 아니다.** 정체는 **독립 CLI 바이너리 + Rust 라이브러리 + 언어별 SDK + containerd shim**.

| 분류 | 해당 | 비고 |
|---|---|---|
| 플러그인 | ✕ | 호스트 앱 확장이 아님 |
| Claude Skill | ✕ | `SKILL.md` 없음 (단, 래핑은 쉬움) |
| MCP 서버 | ✕ | MCP 미구현. 대신 자체 REST + SSE API |
| CLI 도구 | ○ | `smolvm machine / pack / serve` |
| Rust 크레이트 | ○ | `[lib] name = "smolvm"`, crates.io 퍼블리시 워크플로 존재 |
| 임베디드 SDK | ○ | Node(napi), Python(rollout) |
| containerd shim v2 | ○ | 쿠버네티스 RuntimeClass |

대신 **에이전트 친화적**으로 설계돼 있다 — `AGENTS.md` 615줄, README에도
`curl … | bash && smolvm --help  # for coding agents` 라고 명시.

### 확장 기회 (공식 구현이 없는 영역)

1. **smolvm MCP 서버** — `create_sandbox / exec / upload_file / branch / destroy` 툴 노출. HTTP API 위에 얇은 래퍼면 충분. **선점 가치 있음**
2. **Claude Skill 래핑** — `SKILL.md` 하나로 가능
3. **Claude Code 플러그인** — 슬래시 커맨드 + 훅으로 샌드박스 실행 강제

---

## 8. API 토큰이 필요한가?

**smolvm 자체는 토큰·계정이 전혀 필요 없다(완전 로컬).**

| 상황 | 토큰 | 비고 |
|---|---|---|
| 퍼블릭 이미지 pull | 불필요 | |
| 프라이빗 레지스트리 pull | 필요 | `smolvm config registries edit` |
| ghcr 등에 pack push | 필요 | 레지스트리 자격증명 |
| `.smolmachine` 로컬 / `docker save` 아카이브 | 불필요 | 완전 오프라인 |
| 워크로드 내부에서 LLM API 호출 | 필요 | `--secret-env` 로 주입 |
| smolmachines.com 클라우드 | 필요 | 별개의 상용 서비스 |

### 시크릿 설계 (배울 점)

smolvm은 **비밀값을 저장하지 않고, 호스트 상의 위치를 가리키는 "참조"만 저장**한다.

```bash
smolvm machine run --secret-env OPENAI_API_KEY=OPENAI_API_KEY -- ./app
smolvm machine run --secret-file GCP_CREDS=/abs/creds.json -- ./app
op run --env-file=secrets.env -- smolvm machine run -- ./app   # 외부 매니저 브릿지
```
```toml
[secrets]
DATABASE_URL = { from_env  = "PROD_DB_URL" }
GCP_CREDS    = { from_file = "/abs/creds.json" }
```

1. 참조만 저장 → DB, VM 레코드, `.smolmachine` 팩에 평문이 들어가지 않음
2. **늦은 바인딩** → 값 로테이션이 다음 실행에 자동 반영
3. **신뢰 경계 구분** → HTTP API 요청 본문과 포터블 팩은 신뢰하지 않는 호출자로 보고 시크릿 참조를 **거부** (서버 환경변수 유출 / 임의 파일 읽기 방지)

한계도 문서에 명시돼 있다: 대상 프로세스는 자기 환경변수에서 평문을 보고, 게스트 root는 `/proc/*/environ`을 읽을 수 있다. **절대 유출 금지가 필요하면 SSH 에이전트 포워딩**을 쓴다 — 개인키는 호스트에 남고 게스트는 서명 요청만 가능하므로 게스트 root도 키를 추출할 수 없다.

---

## 9. 왜 GitHub에서 유명한가

(스타 수는 이 세션에서 직접 확인하지 않았으므로 수치는 단정하지 않음. 구조적 이유만 정리.)

1. **타이밍** — "LLM이 생성한 코드를 어디서 실행하나"가 현재 최대 화두. E2B/Modal/Daytona가 겨루는 시장을 **로컬에서 무료로** 해결
2. **비직관적 스펙** — "하드웨어 격리 VM인데 부팅 200ms 미만"이 그 자체로 후킹
3. **macOS 네이티브 + 데몬 없음** — Docker Desktop의 무게/유료화/VM-in-VM 문제를 정면 해결
4. **`.smolmachine`** — "실행 중 VM을 파일 하나로 봉인 후 다른 머신에서 부활"이라는 새로운 개념. 비교표에서 유일하게 지원
5. **`branch` 데모의 화제성** — "켜져 있는 브라우저 8개를 0.1초에 복제". `examples/headless-browser`에 `--no-zygote` 같은 실전 함정까지 문서화
6. **GPU** — Venus Vulkan + CUDA 원격화. `demo/`에 H100 QLoRA 실측 (N=8에서 GPU 메모리 61.8GB → 28.2GB, −54%; N=16에서 컨테이너 대비 처리량 +36%)
7. **문서의 정직함** — `Known Limitations`에 못 하는 것 나열, 보안 모델에 "강화된 멀티테넌트 제어판이 아님"·"릴리스 미서명" 자백, `demo/BENCHMARKS.md`에서 **자기 벤치마크를 스스로 정정**(fork 0.43초는 최소 디스크 기준, 14GB 베이크 시 26~30초), `QA-LOG.md`에 결함 기록
8. **장난감이 아닌 인프라** — containerd shim v2 + RuntimeClass + CRI 적합성 CI + deb/arch/nix/yum 패키징 + OpenAPI
9. **Apache-2.0 + 오픈코어** — 후원은 GitHub Sponsors(BinSquare), 수익은 별도 클라우드

---

## 10. 로컬 에이전트 구축에 도움이 되는가 → 매우 그렇다

| # | 에이전트 요구사항 | smolvm 제공 |
|---|---|---|
| 1 | 코드 실행 샌드박스 | 하드웨어 격리 VM. `AGENTS.md`에 "Typical agent workflow" 예시 명시 |
| 2 | 프롬프트 인젝션 → 유출 방어 | `--allow-host` 화이트리스트 + DNS 필터. 인젝션이 성공해도 나갈 경로가 없음 |
| 3 | 병렬 fan-out | `branch --count N --parallel N`. 의존성·모델 로드는 golden에서 한 번만 |
| 4 | 워커 풀 관리 | `src/pool.rs`, `api/pool_controller.rs`, `api/admission.rs` — 자동 fork 풀 + 리스 + TTL + 승인제어 |
| 5 | 되감기(rewind) | `.smolcheckpoint` 복원 → 에이전트 트리 서치 가능 |
| 6 | 원격 오케스트레이션 | REST + SSE (`exec/stream`, `logs`, `files/*path`) |
| 7 | 앱 내 임베드 | Node SDK `quickExec` / `withMachine` / `Machine` (데몬 불필요) |
| 8 | 브라우저·컴퓨터 유즈 | `examples/headless-browser` (GPU Chromium 풀), `examples/desktop` (Hyprland + VNC, 키보드/마우스 주입) |

### 실전 워크플로 (AGENTS.md 발췌)

```bash
smolvm machine create --name r-sandbox --image r-base:latest --net
smolvm machine start  --name r-sandbox
smolvm machine cp analysis.R r-sandbox:/workspace/analysis.R
smolvm machine exec --name r-sandbox -- Rscript /workspace/analysis.R
smolvm machine cp r-sandbox:/workspace/results.csv ./results.csv
smolvm machine stop --name r-sandbox
```

### 주의사항

- 에이전트의 **두뇌(LLM 호출/플래닝)는 직접 만들어야 한다.** smolvm은 실행 환경만 제공
- Windows는 branch / checkpoint / GPU 미지원
- 보안 모델: 호스트 사용자 권한으로 동작. **적대적 로컬 공동 테넌트**용이 아님
- CUDA는 Driver API만. PyTorch/nvcc(Runtime API) 미지원
- GPU 격리는 프로세스 레벨 — 하드닝된 멀티테넌트 GPU 경계로 취급 금지
- 임베디드 Node SDK는 현재 DB를 거치지 않아 CLI에 머신이 보이지 않는 **알려진 버그**가 있음 → 프로덕션은 HTTP API 경로가 안전

---

## 11. React / PHP로 만들 수 있는가

### smolvm 자체 재현 → 불가능

KVM ioctl / Hypervisor.framework / WHP 직접 호출, 게스트 커널 부팅, virtio 디바이스 구현, CoW memfd fork, Mach-O 조작·코드서명 — 모두 네이티브 시스템 프로그래밍(Rust/C) 영역이다.

### smolvm을 엔진으로 쓰는 제품 → 완전히 가능 (권장 방향)

```
React 프론트엔드   (샌드박스 목록 · xterm.js 터미널 · 파일탐색기 · SSE 실시간 로그 · 사용량 대시보드)
        │ REST / SSE / WebSocket
백엔드 (PHP Laravel 또는 Node)   (인증 · 사용자관리 · 결제 · 쿼터 · 감사로그)
        │ HTTP (Guzzle / fetch)
smolvm serve start --listen 127.0.0.1:8080     ← 기성품, 수정 불필요
```

#### PHP (Laravel) 예시

```php
class Smolvm
{
    private string $base = 'http://127.0.0.1:8080/api/v1';

    public function create(string $name, string $image, array $allowHosts = []): array
    {
        return Http::post("{$this->base}/machines", [
            'name'    => $name,
            'image'   => $image,
            'net'     => true,
            'network' => ['allow_hosts' => $allowHosts],
        ])->json();
    }

    public function start(string $name): void
    {
        Http::post("{$this->base}/machines/{$name}/start");
    }

    public function exec(string $name, array $cmd): array
    {
        return Http::timeout(300)
            ->post("{$this->base}/machines/{$name}/exec", ['command' => $cmd])
            ->json();
    }

    public function upload(string $name, string $path, string $content): void
    {
        Http::withBody($content, 'application/octet-stream')
            ->put("{$this->base}/machines/{$name}/files/{$path}");
    }

    public function destroy(string $name): void
    {
        Http::delete("{$this->base}/machines/{$name}");
    }
}
```

```php
Route::post('/run', function (Request $r, Smolvm $vm) {
    $name = 'u'.auth()->id().'-'.Str::random(6);
    $vm->create($name, 'python:3.12-alpine', ['pypi.org']);
    $vm->start($name);
    $vm->upload($name, 'workspace/main.py', $r->input('code'));
    $out = $vm->exec($name, ['python3', '/workspace/main.py']);
    $vm->destroy($name);
    return $out;      // {stdout, stderr, exitCode}
});
```

#### React 예시 (SSE 스트리밍)

```tsx
function SandboxTerminal({ name }: { name: string }) {
  const [lines, setLines] = useState<string[]>([]);

  useEffect(() => {
    const es = new EventSource(`/api/sandbox/${name}/logs`);
    es.addEventListener("stdout", (e) => setLines((p) => [...p, e.data]));
    es.addEventListener("exit", (e) => {
      const { exitCode } = JSON.parse(e.data);
      setLines((p) => [...p, `── exited with ${exitCode} ──`]);
      es.close();
    });
    return () => es.close();
  }, [name]);

  return (
    <pre className="bg-zinc-900 text-lime-400 p-4 rounded-xl overflow-auto">
      {lines.join("\n")}
    </pre>
  );
}
```

#### Node / Next.js (임베디드 SDK)

```ts
import { withMachine } from "smolvm-embedded";

export async function POST(req: Request) {
  const { code } = await req.json();
  return withMachine({ name: `run-${crypto.randomUUID()}` }, async (sb) => {
    const r = await sb.exec(["python3", "-c", code]);
    return Response.json({ stdout: r.stdout, exit: r.exitCode });
  });
}
```

| 목표 | 가능 |
|---|---|
| smolvm 재구현 | 불가 (Rust/C 영역) |
| React 관리 UI / 웹 IDE / 터미널 | 가능 |
| PHP SaaS 백엔드 (인증·결제·쿼터) | 가능 |
| Node/Next.js 임베드 | 가능 (가장 쉬움) |
| MCP 서버 (TS/Python) | 가능 (가장 쉬움) |

**핵심:** 엔진은 이미 무료로 주어졌고, 수익은 그 위의 제품 레이어에서 발생한다. 그 레이어가 정확히 React/PHP의 영역이다.

---

## 12. 수익화 아이디어

### 법적 전제

- **Apache-2.0** → 상업적 이용·수정·재배포·SaaS 제공 모두 합법. 라이선스/저작권 고지 및 NOTICE 유지 필요
- 주의: ① "smolvm/smolmachines" **상표는 사용하지 않을 것** ② 업스트림도 smolmachines.com 유료 클라우드를 운영 중이므로 정면 충돌 회피 ③ 번들 네이티브 라이브러리(libkrun/libkrunfw/MoltenVK) 라이선스 별도 확인 (`Licenses.md`)

### 아이디어 목록

**1. 에이전트 샌드박스 SaaS** — 난이도 ★★★★ / 수익성 ★★★★★
`POST /v1/sandboxes` 형태로 격리 실행을 API로 판매. 타깃은 AI 에이전트 스타트업·코드 인터프리터 팀. 실행 초당 과금 + 월 정액($29/$199/엔터프라이즈). `branch` 기반 워밍업 풀로 콜드스타트 원가를 구조적으로 낮출 수 있는 것이 마진 우위. 리스크는 인프라 원가·운영 부담과 업스트림 클라우드와의 경쟁.

**2. 로컬 에이전트 보안 게이트웨이** — 난이도 ★★ / 수익성 ★★★★ → **최우선 추천**
Claude Code / Cursor / Copilot이 생성한 코드·명령을 자동으로 smolvm 안에서 실행시키는 로컬 제품. Electron/Tauri 데스크탑 앱 + CLI + MCP/플러그인. 로컬 대시보드(React)로 실행 이력·차단 로그·되감기 제공. 개인 $12~19/월, 팀 $39/시트/월, 기업 온프렘 라이선스.
→ **인프라 원가 0**(사용자 머신에서 실행), 만들 부분이 정확히 UI·정책 레이어.

**3. smolvm MCP 서버 + 오픈코어** — 난이도 ★ / 수익성 ★★
무료 MCP 서버 공개로 생태계 진입 + 유입 깔때기. 유료 Pro에 감사 로그, 팀 정책 중앙관리, SSO, 프리워밍 풀, 리플레이. 공식 구현이 없어 선점 가치.

**4. 브라우저 자동화 / 스크래핑 API** — 난이도 ★★★ / 수익성 ★★★
`examples/headless-browser`의 fork 트릭으로 세션 시작 1~3초 → 90ms. Browserbase/Browserless 대체. 원가 구조상 가격 경쟁력 확보 가능.

**5. `.smolmachine` 환경 마켓플레이스** — 난이도 ★★★ / 수익성 ★★
"R+생물정보학", "CUDA+PyTorch+Jupyter", "레거시 PHP 5.6 + MySQL 5.5 재현" 같은 프리미엄 환경을 개당 $19~99 또는 구독으로 판매. 부수적으로 대학·부트캠프에 "환경 세팅 0분"으로 판매 가능.

**6. 교육 플랫폼 / 코딩 채점 서비스** — 난이도 ★★★ / 수익성 ★★★
온라인 코테, 과제 자동채점, 실습형 강의. 학교·기업 B2B 연간 라이선스. 도커 기반 경쟁사 대비 "커널 격리"가 세일즈 포인트. 한국 시장 수요 탄탄.

**7. 온프렘 / 규제산업 컨설팅 + 서포트** — 난이도 ★★ / 수익성 ★★★★ (단가 최고)
금융·의료·공공의 에어갭 환경. `docker save` 아카이브로 완전 오프라인 부팅 가능하고 k8s shim으로 기존 클러스터에 얹기 쉽다. 도입 컨설팅 + 커스터마이징 + 연간 지원계약.

**8. RL / LoRA 학습 인프라** — 난이도 ★★★★★ / 수익성 ★★★★★
`demo/` 실측 근거(H100, N=8에서 GPU 메모리 −54%, N=16에서 처리량 +36%)로 "같은 GPU로 2배 학습" 세일즈. 단 CUDA Runtime API 미지원 등 기술 난이도 최상, GPU 서버 자본 필요.

### 종합 비교

| # | 아이디어 | 난이도 | 초기자본 | 수익 잠재력 | React/PHP 적합도 | 점수 |
|---|---|---|---|---|---|---|
| 2 | 로컬 에이전트 보안 게이트웨이 | ★★ | 거의 0 | ★★★★ | 매우 높음 | **9.5** |
| 3 | MCP 서버 + 오픈코어 | ★ | 0 | ★★ | 매우 높음 | **9.0** |
| 7 | 온프렘 컨설팅/지원 | ★★ | 0 | ★★★★ | 보통 | **8.5** |
| 6 | 교육/채점 플랫폼 | ★★★ | 중 | ★★★ | 매우 높음 | 8.0 |
| 1 | 샌드박스 SaaS | ★★★★ | 높음 | ★★★★★ | 높음 | 7.5 |
| 4 | 브라우저 자동화 API | ★★★ | 중 | ★★★ | 높음 | 7.0 |
| 5 | `.smolmachine` 마켓 | ★★★ | 낮음 | ★★ | 매우 높음 | 6.5 |
| 8 | RL 학습 인프라 | ★★★★★ | 매우 높음 | ★★★★★ | 낮음 | 6.0 |

### 실행 로드맵 (제안)

1. **1~2주차 — MCP 서버 (무료)**: `smolvm serve` 위 얇은 래퍼. `create_sandbox / exec / upload / branch / destroy`. 공개 후 커뮤니티 홍보로 신뢰·유입 확보
2. **3~8주차 — 로컬 게이트웨이 MVP (유료화)**: Tauri/Electron + React 대시보드, 정책 프리셋("npm만"/"pypi만"/"완전 오프라인"), 실행 이력·차단 로그·되감기, Claude Code/Cursor 훅 연동. 얼리버드 $15/월
3. **3개월차 — B2B 확장**: 팀 기능(정책 중앙관리·감사로그·SSO) $39/시트. 동시에 온프렘 컨설팅 문의 수령 (현금흐름)
4. **6개월차 — 분기 선택**: 수요가 크면 클라우드 샌드박스 SaaS로 확장, 니치면 교육·규제산업 특화

### 리스크와 대응

| 리스크 | 대응 |
|---|---|
| 업스트림이 동일 영역 상용화 | 상보적 포지션(로컬 보안 레이어) 선택 또는 컨트리뷰터로 협력 |
| 업스트림 개발 중단 | Apache-2.0 포크 가능. libkrun은 containers 조직(Podman)이 유지 → 리스크 낮음 |
| 대형 벤더 진입 | 니치·규제산업·국내 시장 선점, 스위칭 비용 형성 |
| 기술 한계(Win branch/GPU, CUDA Runtime) | 초기에는 macOS/Linux + CPU 워크로드에 집중 |
| 상표/라이선스 | 자체 브랜드 사용, NOTICE 유지, 법률 검토 1회 |

---

## 13. 최종 결론

- smolvm은 **"컨테이너의 속도 + VM의 격리"** 를 실제로 성립시킨 Rust 기반 microVM 런타임이며, AI 에이전트 시대의 코드 실행 문제를 정확히 겨냥한다.
- 이 저장소는 그 업스트림의 **포크**이고, 추가된 것은 `CLAUDE.md` 하나다.
- 플러그인·스킬·MCP가 아니라 **독립 CLI + 라이브러리 + SDK**이며, MCP 서버는 아직 공식 구현이 없어 **직접 만들 여지가 있다.**
- smolvm 자체를 React/PHP로 재현하는 것은 불가능하지만, **그 위의 제품 레이어(웹 UI·SaaS 백엔드·MCP)는 React/PHP/Node로 충분히 만들 수 있고**, 수익은 바로 그 레이어에서 발생한다.
- 보유 스택과 자본 규모를 고려하면 **"MCP 서버로 유입 → 로컬 에이전트 보안 게이트웨이로 과금"** 조합이 가장 현실적인 경로다.
