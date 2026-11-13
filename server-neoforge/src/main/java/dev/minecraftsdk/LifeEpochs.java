package dev.minecraftsdk;

import java.lang.ref.WeakReference;
import java.util.Map;
import java.util.UUID;
import java.util.WeakHashMap;

/** The protocol connection and numeric entity ID both survive ordinary respawn. */
final class LifeEpochs {
    private static final Map<Object,Entry> lives=new WeakHashMap<>();
    private record Entry(WeakReference<Object> player,String epoch) {}

    static synchronized String of(Object connection,Object player){
        var old=lives.get(connection);
        // Entity equality may compare numeric IDs; only the actual player
        // object distinguishes two lives that Minecraft assigned the same ID.
        if(old!=null&&old.player().get()==player)return old.epoch();
        var epoch=UUID.randomUUID().toString();
        // A strong player value would retain its connection and defeat weak-key
        // cleanup. The live player list already retains the current player.
        lives.put(connection,new Entry(new WeakReference<>(player),epoch));
        return epoch;
    }
    private LifeEpochs(){}
}
