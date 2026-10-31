import os,sys,time
from pathlib import Path
if len(sys.argv)>2 and os.fork()==0:
 os.setsid()
 for fd in [0,1,2]:
  n=os.open(os.devnull,os.O_RDWR);os.dup2(n,fd);os.close(n)
 path=Path(sys.argv[2])
else:path=Path(sys.argv[1])
end=time.monotonic()+8
with path.open("a",buffering=1) as f:
 while time.monotonic()<end:f.write("tick\n");time.sleep(.02)
