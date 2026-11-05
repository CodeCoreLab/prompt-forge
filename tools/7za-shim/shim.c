/*
 * 7za shim for electron-builder's app-builder on Windows.
 *
 * Problem: the winCodeSign archive ships macOS symlinks. Creating symlinks on
 * Windows requires SeCreateSymbolicLinkPrivilege (Developer Mode), otherwise
 * 7-Zip exits with code 2 and app-builder treats the download as failed and
 * retries forever.
 *
 * Fix: forward every argument to the real 7-Zip and append an exclusion for the
 * "darwin" subtree, which only holds macOS openssl binaries and is irrelevant
 * for Windows packaging. Everything else (rcedit, signtool) extracts normally.
 *
 * Build: gcc -O2 -o 7za.exe shim.c
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define MAX_ARGS 64

int main(int argc, char **argv)
{
    char full[MAX_PATH];
    char cmdline[32768];
    size_t used = 0;
    int i;

    /* Locate the real 7-Zip next to this shim: <dir>\7za.real.exe (installed
       into node_modules) or <dir>\real-7za.exe (standalone usage). */
    DWORD n = GetModuleFileNameA(NULL, full, MAX_PATH);
    if (n == 0 || n >= MAX_PATH) {
        fprintf(stderr, "shim: cannot resolve own path\n");
        return 2;
    }
    char *slash = strrchr(full, '\\');
    if (slash == NULL) {
        fprintf(stderr, "shim: unexpected path\n");
        return 2;
    }
    *slash = '\0';
    char exe[MAX_PATH];
    snprintf(exe, sizeof(exe), "%s\\7za.real.exe", full);
    if (GetFileAttributesA(exe) == INVALID_FILE_ATTRIBUTES) {
        snprintf(exe, sizeof(exe), "%s\\real-7za.exe", full);
    }
    if (GetFileAttributesA(exe) == INVALID_FILE_ATTRIBUTES) {
        fprintf(stderr, "shim: real 7-Zip not found next to shim\n");
        return 2;
    }

    cmdline[0] = '\0';
    used += (size_t)snprintf(cmdline + used, sizeof(cmdline) - used, "\"%s\"", exe);
    for (i = 1; i < argc; i++) {
        used += (size_t)snprintf(cmdline + used, sizeof(cmdline) - used, " \"%s\"", argv[i]);
    }
    /* Skip the macOS-only subtree that requires symlink creation. */
    snprintf(cmdline + used, sizeof(cmdline) - used, " -x!darwin\\*");

    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof(si));
    si.cb = sizeof(si);

    if (!CreateProcessA(exe, cmdline, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        fprintf(stderr, "shim: failed to launch real 7-Zip (%lu)\n", GetLastError());
        return 2;
    }
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return (int)code;
}
