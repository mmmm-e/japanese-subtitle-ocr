$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
python -m venv .venv
if ($LASTEXITCODE) { throw 'Python venv creation failed' }
& .\.venv\Scripts\python.exe -m pip install torch==2.11.0 torchvision==0.26.0 --index-url https://download.pytorch.org/whl/cu128
if ($LASTEXITCODE) { throw 'CUDA PyTorch installation failed' }
& .\.venv\Scripts\python.exe -m pip install -r worker\requirements.txt
if ($LASTEXITCODE) { throw 'OCR dependencies installation failed' }
& .\.venv\Scripts\python.exe -c "import torch; print('PyTorch:',torch.__version__); print('CUDA build:',torch.version.cuda); print('CUDA available:',torch.cuda.is_available()); print('Device:',torch.cuda.get_device_name(0) if torch.cuda.is_available() else 'CPU')"
