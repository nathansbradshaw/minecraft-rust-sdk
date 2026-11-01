package dev.minecraftsdk;
import net.minecraft.server.level.ServerPlayer;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.ServerChatEvent;
import java.util.*;
/** Bounded, per-character chat observation. Never supplies another character's private state. */
final class NativeChat {
    private static final Map<UUID,ArrayDeque<Map<String,Object>>> own=new HashMap<>();
    private static long sequence;
    static void install(){NeoForge.EVENT_BUS.addListener(EventPriority.LOWEST,false,(ServerChatEvent event)->{
        if(SdkService.active==null||!SdkService.active.owns(event.getPlayer()))return;
        var queue=own.computeIfAbsent(event.getPlayer().getUUID(),id->new ArrayDeque<>());
        queue.addLast(Map.of("sequence",++sequence,"message",event.getRawText()));while(queue.size()>16)queue.removeFirst();
    });}
    static List<Map<String,Object>> snapshot(ServerPlayer player){return List.copyOf(own.getOrDefault(player.getUUID(),new ArrayDeque<>()));}
    private NativeChat(){}
}
