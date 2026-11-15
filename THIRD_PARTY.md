# Third-party components

이 저장소의 자체 소스 코드는 [MIT License](LICENSE)로 제공합니다. 외부 라이브러리, OCR 모델 가중치, FFmpeg, Python 및 GPU 런타임의 라이선스는 각 구성요소의 조건을 따릅니다. 프로젝트의 MIT 표기가 이들 구성요소의 라이선스를 변경하지 않습니다.

| 구성요소 | 출처 / 라이선스 확인 |
| --- | --- |
| PaddleOCR-VL 1.6 모델 | [공식 모델 카드](https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6) — Apache-2.0 표기 |
| Manga OCR CPT 모델 | [공식 모델 저장소](https://huggingface.co/muscgab/manga-ocr-base-CPT)의 모델 카드 및 라이선스 확인 |
| manga-ocr | [공식 저장소](https://github.com/kha-white/manga-ocr)의 라이선스 확인 |
| FFmpeg / FFprobe | [공식 라이선스 안내](https://ffmpeg.org/legal.html). 별도 설치한 배포본의 구성과 조건 확인 |
| Rust 의존성 | `Cargo.lock`의 버전 및 각 crate의 라이선스 확인 |
| Python 의존성 | `worker/requirements*.txt`의 버전 및 각 패키지의 라이선스 확인 |

가상환경, 모델 가중치, FFmpeg 바이너리는 소스 저장소에 포함하지 않습니다. 향후 설치 프로그램이나 바이너리 묶음을 배포할 때는 실제 포함하는 구성요소에 맞춰 라이선스 및 고지 파일을 제공해야 합니다.
