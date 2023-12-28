# Hazard Pointer

A technology used for lock-free algorithm.

## Preface

Assume we want to operate a variable in a multiple thread which can be read and written at the same time.

In practice, we usually use AtomicPtr to solve this problem:

```rust
struct Node {}

struct DataStructure {
	ptr: AtomicPtr<Node>,
}
```

Which means, we put the Node on the heap, and make the AtomicPtr point to the Node. So when we update the Node, what we actually do is swap the pointer, make the pointer point to another address. But in realistic, when we in non-gc-based language, we have to reclaim the memory manually. So then things become difficult, when can i free the memory? Since its apparently, someone else may still read the memory.

Think about a case, a write thread swap the AtomicPtr to Node2, but there has a reader already has the pointer to Node1, before the swap, now what the reader should do? 

The solution of hazard pointer is each thread hold a hazard pointer, when the thread A, the reader access the Node1, the reader should put the address of the Node1 into the hazard pointer. The address is the actually memory address of Node1 rather than the address of AtomicPtr. 

So when thread B, the writer, who is doing the swap, it should retry to check if another thread contains the value it is try to swap, which means, it look ups all hazard pointers of all threads to check if contains the address of Node1. Then it find thread A has a hazard pointer is guarding the address right now, so which means the writer cannot do the deallocate at the moment. 

Some time passes, the thread A doesn’t access the Node1 any more, then it should unmark the access of Node1 from the hazard pointer; so then threadB comes back and check the hazard pointers again, it doesn’t find any occurrence of address of Node1 is guarded by any hazard pointers, therefore, it can actually deallocate the Node1. Since after the swap, there will be no one can access the old Node1, and now no one say they need the Node1, so we can actually wipe away the memory.

## Compare with epoch-based reclamation

Epoch-based will be more efficient than hazard pointer since it makes deallocate in a bunch way, but it also lead to a bit more memory cost, since you cannot free until one epoch end.

## How one thread knows another thread is holding the memory of Node1 ?

The writer need to know all about the reader hazard pointers, otherwise it doesn’t know how to look up in the first place. 

So the hazard pointers are basically going to constitute a long linkedlist, which will be a lock-free linkedlist, that just linked all hazard pointers. And its a thread shared linkedlist, when one thread is going to read, it should grab an unused hazard pointer from the shard linkedlist, or there isn’t we allocate one. If you are a writer, then we walk the linkedlist and check all of them.

## How to solve the problem of reclamation of the hazard pointer it self ?

Good news is that we don’t deallocate, then reader thread give up access, we just mark the hazard pointer as inactive for reusing, so we kind avoid the problem, then we can just use standard atomic linkedlist.

## Won’t each reader need to create each node for reading ?

As a reader, it is true we have to have a hazard pointer in order to read, but it is on a linkedlist, we can grab an unused node or create one, and in these scenario, there won’t be much contention; compare to reference counting,  we are not liking to have multiple threads try to compare and swap at the single value. 

It is true that there may have a little bit contention,  but it won’t allocate new hazard pointer each time.

## Race condition introduce

Think about the case:

1. Reader read the pointer X
2. Writer swap the pointer from X  to Y
3. Writer check the hazard pointer
    1. writer find out nothing since the reader haven’t put the pointer into the hazard pointer yet.
    2. writer drop the origin X
4. Reader put the pointer into Hazard pointer and then read.

The way to avoid this is double read, when the reader fetch the address and write it into the hazard pointer, the reader have to fetch the address again in case of existing writer change the address.

## What do you do when you cannot do reclamation right now?

The hazard pointer way is declaring a retirement list, which stores the memory we need to reclamation, and each time we call the retire on some object, we first put the object to the retirement list, and walk through the retirement list to check weather exists some object is not guarded anymore, then we can free those objects.

## What to implement ?

1. the linkedlist of hazard pointers
2. the guarding of readers have to do
3. the writers have to walk through the linkedlist