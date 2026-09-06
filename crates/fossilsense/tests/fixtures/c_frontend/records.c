struct Packet;
struct Packet { int count; int (*send)(int); };
typedef struct Packet PacketAlias;
