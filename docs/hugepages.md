# Backing Guest Memory by Huge Pages

> [!WARNING]
> Support is currently in **developer preview**. See
> [this section](#developer-preview-status) for more info.

Firecracker supports backing the guest memory of a VM by 2MB hugetlbfs
pages. This can be enabled by setting the `huge_pages` field
of `PUT` or `PATCH` requests to the `/machine-config` endpoint to `2M`.

Using hugetlbfs requires the host running Firecracker to have a pre-allocated
pool of 2M pages. Should this pool be too small, Firecracker may behave erratically
or receive the `SIGBUS` signal. For details on how to manage this pool, please
refer to the [Linux Documentation](https://docs.kernel.org/admin-guide/mm/hugetlbpage.html).

## Known Limitations

Currently, hugetlbfs support is mutually exclusive with the following
Firecracker features:

- Memory Ballooning via the [Balloon Device](./ballooning.md)
- Differential Snapshots
- Initrd

## FAQ

### Why does Firecracker not offer a transparent huge pages (THP) setting?

Firecracker's guest memory is memfd based. Linux does not offer a way
to dynamically enable THP for such memory regions.
The only way to have memfd-backed memory utilize THP is by setting the
`/sys/kernel/mm/transparent_hugepage/shmem_enabled`
sysfs to `always`, which will enable THP globally for all shared memory
region/tmpfses on the host.
