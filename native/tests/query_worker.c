// A disposable query worker: no subprocesses, user data, or service interaction.
// Ignore TERM so the menu must exercise its bounded KILL fallback and reap us.
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 3 || strcmp(argv[argc - 2], "--ready") != 0) return 2;
    if (signal(SIGTERM, SIG_IGN) == SIG_ERR) return 3;
    alarm(20); // Bound fixture lifetime even if the parent test aborts.
    int file = open(argv[argc - 1], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (file < 0) return 4;
    char ready[40];
    int length = snprintf(ready, sizeof ready, "%ld\n", (long)getpid());
    ssize_t written;
    do { written = write(file, ready, (size_t)length); } while (written < 0 && errno == EINTR);
    if (close(file) != 0 || written != length) return 5;
    for (;;) pause();
}
