"""Protocol fixture, not an OCR backend exposed to users."""
import json
import os
from pathlib import Path
import sys
import time
count = 0
model = "mock"
for line in sys.stdin:
    req = json.loads(line)
    op = req['op']
    if op == 'shutdown':
        break
    if op == 'init':
        model = req.get('model', 'mock')
    result = {'model':model, 'pid':os.getpid(),
              'inference':{'max_pixels':req.get('max_pixels')}, 'revision':'v1', 'device':'cpu',
              'capabilities':{'input':'cropped_text_image','gpu':False,'confidence':False,
                              'max_concurrency':1,'internal_grayscale':False,'multiline_reliable':False}}
    if op == 'ocr':
        name = Path(req['image']).stem
        if name == 'exit':
            os._exit(7)
        if name == 'cancel':
            time.sleep(30)
        if name == 'error':
            print(json.dumps({'id':req['id'],'ok':False,'error':'controlled model failure'}),flush=True)
            continue
        if name == 'bad_id':
            req['id'] += 1
        count += 1
        result.update(text=str(count),elapsed_ms=1,confidence=None)
    print(json.dumps({'id':req['id'],'ok':True,'result':result}),flush=True)
