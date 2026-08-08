// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include <errno.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#ifdef _WIN32
#include <io.h>
#define S_IFDIR _S_IFDIR
#define S_IFREG _S_IFREG
#define W_OK 2
#else
#include <unistd.h>
#endif

#include "tcl.h"

extern size_t opto_tcl_embedded_entry_count(void);
extern const char *opto_tcl_embedded_entry_path(size_t index);
extern const unsigned char *opto_tcl_embedded_entry_data(size_t index);
extern size_t opto_tcl_embedded_entry_length(size_t index);
extern int opto_tcl_embedded_entry_is_dir(size_t index);

typedef struct OptoMemoryChannel {
    const unsigned char *data;
    size_t length;
    size_t position;
} OptoMemoryChannel;

static int opto_channel_close(ClientData instance_data, Tcl_Interp *interp) {
    (void)interp;
    free(instance_data);
    return TCL_OK;
}

static int opto_channel_input(ClientData instance_data, char *buffer,
                              int bytes_to_read, int *error_code) {
    OptoMemoryChannel *channel = (OptoMemoryChannel *)instance_data;
    size_t remaining;
    size_t count;

    (void)error_code;
    if (bytes_to_read <= 0 || channel->position >= channel->length) {
        return 0;
    }
    remaining = channel->length - channel->position;
    count = remaining < (size_t)bytes_to_read ? remaining : (size_t)bytes_to_read;
    memcpy(buffer, channel->data + channel->position, count);
    channel->position += count;
    return (int)count;
}

static int opto_channel_seek(ClientData instance_data, long offset, int mode,
                             int *error_code) {
    OptoMemoryChannel *channel = (OptoMemoryChannel *)instance_data;
    Tcl_WideInt base;
    Tcl_WideInt next;

    switch (mode) {
    case SEEK_SET:
        base = 0;
        break;
    case SEEK_CUR:
        base = (Tcl_WideInt)channel->position;
        break;
    case SEEK_END:
        base = (Tcl_WideInt)channel->length;
        break;
    default:
        *error_code = EINVAL;
        return -1;
    }
    next = base + (Tcl_WideInt)offset;
    if (next < 0 || (uint64_t)next > (uint64_t)channel->length ||
        next > INT_MAX) {
        *error_code = EINVAL;
        return -1;
    }
    channel->position = (size_t)next;
    return (int)next;
}

static Tcl_WideInt opto_channel_wide_seek(ClientData instance_data,
                                          Tcl_WideInt offset, int mode,
                                          int *error_code) {
    OptoMemoryChannel *channel = (OptoMemoryChannel *)instance_data;
    Tcl_WideInt base;
    Tcl_WideInt next;

    switch (mode) {
    case SEEK_SET:
        base = 0;
        break;
    case SEEK_CUR:
        base = (Tcl_WideInt)channel->position;
        break;
    case SEEK_END:
        base = (Tcl_WideInt)channel->length;
        break;
    default:
        *error_code = EINVAL;
        return -1;
    }
    next = base + offset;
    if (next < 0 || (uint64_t)next > (uint64_t)channel->length) {
        *error_code = EINVAL;
        return -1;
    }
    channel->position = (size_t)next;
    return next;
}

static void opto_channel_watch(ClientData instance_data, int mask) {
    (void)instance_data;
    (void)mask;
}

static int opto_channel_get_handle(ClientData instance_data, int direction,
                                   ClientData *handle) {
    (void)instance_data;
    (void)direction;
    (void)handle;
    return TCL_ERROR;
}

static int opto_channel_block_mode(ClientData instance_data, int mode) {
    (void)instance_data;
    (void)mode;
    return TCL_OK;
}

static const Tcl_ChannelType opto_channel_type = {
    "opto-memory",
    TCL_CHANNEL_VERSION_5,
    opto_channel_close,
    opto_channel_input,
    NULL,
    opto_channel_seek,
    NULL,
    NULL,
    opto_channel_watch,
    opto_channel_get_handle,
    NULL,
    opto_channel_block_mode,
    NULL,
    NULL,
    opto_channel_wide_seek,
    NULL,
    NULL,
};

static const char *opto_path(Tcl_Obj *path_ptr) {
    return Tcl_GetString(path_ptr);
}

static ptrdiff_t opto_find_entry(const char *path) {
    size_t count = opto_tcl_embedded_entry_count();
    size_t index;

    for (index = 0; index < count; ++index) {
        if (strcmp(path, opto_tcl_embedded_entry_path(index)) == 0) {
            return (ptrdiff_t)index;
        }
    }
    return -1;
}

static int opto_path_in_filesystem(Tcl_Obj *path_ptr,
                                   ClientData *client_data) {
    const char *path = opto_path(path_ptr);
    (void)client_data;
    return strncmp(path, "opto:/", 6) == 0 ? TCL_OK : -1;
}

static Tcl_Obj *opto_separator(Tcl_Obj *path_ptr) {
    (void)path_ptr;
    return Tcl_NewStringObj("/", 1);
}

static int opto_stat(Tcl_Obj *path_ptr, Tcl_StatBuf *buffer) {
    ptrdiff_t index = opto_find_entry(opto_path(path_ptr));
    if (index < 0) {
        Tcl_SetErrno(ENOENT);
        return -1;
    }

    memset(buffer, 0, sizeof(*buffer));
    buffer->st_mode = opto_tcl_embedded_entry_is_dir((size_t)index)
                          ? (S_IFDIR | 0555)
                          : (S_IFREG | 0444);
    buffer->st_nlink = 1;
    buffer->st_size = (Tcl_WideInt)opto_tcl_embedded_entry_length((size_t)index);
    return 0;
}

static int opto_access(Tcl_Obj *path_ptr, int mode) {
    if (mode & W_OK) {
        Tcl_SetErrno(EACCES);
        return -1;
    }
    if (opto_find_entry(opto_path(path_ptr)) < 0) {
        Tcl_SetErrno(ENOENT);
        return -1;
    }
    return 0;
}

static Tcl_Channel opto_open(Tcl_Interp *interp, Tcl_Obj *path_ptr, int mode,
                             int permissions) {
    const char *path = opto_path(path_ptr);
    ptrdiff_t index = opto_find_entry(path);
    OptoMemoryChannel *memory;
    Tcl_Channel channel;

    (void)permissions;
    if (index < 0 || opto_tcl_embedded_entry_is_dir((size_t)index)) {
        Tcl_SetErrno(ENOENT);
        return NULL;
    }
    if ((mode & TCL_WRITABLE) != 0) {
        Tcl_SetErrno(EROFS);
        return NULL;
    }

    memory = (OptoMemoryChannel *)malloc(sizeof(*memory));
    if (memory == NULL) {
        Tcl_SetErrno(ENOMEM);
        return NULL;
    }
    memory->data = opto_tcl_embedded_entry_data((size_t)index);
    memory->length = opto_tcl_embedded_entry_length((size_t)index);
    memory->position = 0;

    channel = Tcl_CreateChannel(&opto_channel_type, path, memory, TCL_READABLE);
    if (channel == NULL) {
        free(memory);
        return NULL;
    }
    if (Tcl_SetChannelOption(interp, channel, "-encoding", "utf-8") != TCL_OK ||
        Tcl_SetChannelOption(interp, channel, "-translation", "lf") != TCL_OK) {
        Tcl_Close(NULL, channel);
        return NULL;
    }
    return channel;
}

static int opto_type_matches(size_t index, const Tcl_GlobTypeData *types) {
    int type;
    if (types == NULL) {
        return 1;
    }
    type = opto_tcl_embedded_entry_is_dir(index) ? TCL_GLOB_TYPE_DIR
                                                 : TCL_GLOB_TYPE_FILE;
    if (types->type != 0 && (types->type & type) == 0) {
        return 0;
    }
    if ((types->perm & (TCL_GLOB_PERM_W | TCL_GLOB_PERM_X)) != 0) {
        return 0;
    }
    return 1;
}

static int opto_append_match(Tcl_Interp *interp, Tcl_Obj *result,
                             const char *path) {
    return Tcl_ListObjAppendElement(interp, result, Tcl_NewStringObj(path, -1));
}

static int opto_match_in_directory(Tcl_Interp *interp, Tcl_Obj *result,
                                   Tcl_Obj *path_ptr, const char *pattern,
                                   Tcl_GlobTypeData *types) {
    const char *directory = opto_path(path_ptr);
    size_t directory_length = strlen(directory);
    size_t count = opto_tcl_embedded_entry_count();
    size_t index;

    if (pattern == NULL) {
        ptrdiff_t exact = opto_find_entry(directory);
        if (exact >= 0 && opto_type_matches((size_t)exact, types)) {
            return opto_append_match(interp, result, directory);
        }
        return TCL_OK;
    }

    for (index = 0; index < count; ++index) {
        const char *entry = opto_tcl_embedded_entry_path(index);
        const char *name;
        if (strncmp(entry, directory, directory_length) != 0) {
            continue;
        }
        name = entry + directory_length;
        if (directory_length > 0 && directory[directory_length - 1] != '/') {
            if (*name != '/') {
                continue;
            }
            ++name;
        }
        if (*name == '\0' || strchr(name, '/') != NULL ||
            !Tcl_StringMatch(name, pattern) || !opto_type_matches(index, types)) {
            continue;
        }
        if (opto_append_match(interp, result, entry) != TCL_OK) {
            return TCL_ERROR;
        }
    }
    return TCL_OK;
}

static Tcl_Obj *opto_list_volumes(void) {
    Tcl_Obj *volumes = Tcl_NewListObj(0, NULL);
    Tcl_ListObjAppendElement(NULL, volumes, Tcl_NewStringObj("opto:/", 6));
    return volumes;
}

static const Tcl_Filesystem opto_filesystem = {
    .typeName = "opto-embedded",
    .structureLength = sizeof(Tcl_Filesystem),
    .version = TCL_FILESYSTEM_VERSION_1,
    .pathInFilesystemProc = opto_path_in_filesystem,
    .filesystemSeparatorProc = opto_separator,
    .statProc = opto_stat,
    .accessProc = opto_access,
    .openFileChannelProc = opto_open,
    .matchInDirectoryProc = opto_match_in_directory,
    .listVolumesProc = opto_list_volumes,
    .lstatProc = opto_stat,
};

int opto_tcl_vfs_register(void) {
    return Tcl_FSRegister(NULL, &opto_filesystem);
}
