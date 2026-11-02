// Disposable macOS filesystem-sandbox workload. No credential or user path input.
#include <sys/mman.h>
#include <sys/types.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
int main(int argc, char **argv) {
    if (argc != 5) return 64;
    pid_t child = fork();
    if (child < 0) return 2;
    if (child > 0) { sleep(6); return 0; }
    pid_t detached=setsid();
    size_t bytes = (size_t)strtoul(argv[4], NULL, 10) * 1024 * 1024;
    volatile char *working = calloc(bytes ? bytes : 1, 1);
    if (!working) return 2;
    for (size_t n=0; n<bytes; n+=4096) working[n]=1;
    char path[4096];
    snprintf(path,sizeof(path),"%s/forbidden",argv[2]);
    int forbidden=open(path,O_WRONLY|O_CREAT,0600); int open_errno=errno;
    int inherited=atoi(argv[3]); errno=0;
    ssize_t escaped=write(inherited,"BAD",3); int write_errno=errno;
    snprintf(path,sizeof(path),"%s/attempts.json",argv[1]);
    FILE *report=fopen(path,"w");
    if (report) { fprintf(report,"{\"canonicalOpen\":%d,\"openErrno\":%d,\"inheritedWrite\":%zd,\"writeErrno\":%d,\"pid\":%d,\"setsid\":%d}\n",forbidden,open_errno,escaped,write_errno,getpid(),detached); fclose(report); }
    if (forbidden>=0) close(forbidden);
    snprintf(path,sizeof(path),"%s/held.log",argv[1]);int held=open(path,O_WRONLY|O_CREAT|O_APPEND,0600);
    snprintf(path,sizeof(path),"%s/mmap.bin",argv[1]);int mapped=open(path,O_RDWR|O_CREAT,0600);
    if (held<0 || mapped<0 || ftruncate(mapped,4096)) return 3;
    uint64_t *memory=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,mapped,0);
    if (memory==MAP_FAILED) return 4;
    snprintf(path,sizeof(path),"%s/burst.bin",argv[1]);int bulk=open(path,O_WRONLY|O_CREAT|O_TRUNC,0600);
    char zero[65536]={0};for(int n=0;n<512 && bulk>=0;n++) if(write(bulk,zero,sizeof(zero))!=sizeof(zero))break;
    if(bulk>=0)close(bulk);
    struct timespec pause={0,20000000};
    for(uint64_t n=1;n<=200;n++) { write(held,"HELD_WRITE\n",11);*memory=n;msync(memory,4096,MS_SYNC);nanosleep(&pause,NULL); }
    return 0;
}
