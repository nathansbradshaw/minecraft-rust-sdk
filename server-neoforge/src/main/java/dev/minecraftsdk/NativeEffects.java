package dev.minecraftsdk;

import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.Connection;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.damagesource.DamageTypes;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.entity.living.LivingDamageEvent;
import net.neoforged.neoforge.event.entity.living.LivingEntityUseItemEvent;
import java.util.*;

/** Post-effect facts scoped to the admitted player's actual connection. */
final class NativeEffects {
    private static final Map<Connection,Inbox> inboxes=new WeakHashMap<>();
    private static final class Inbox {
        final String life;
        final String epoch=UUID.randomUUID().toString();
        long sequence;
        final ArrayDeque<Map<String,Object>> rows=new ArrayDeque<>();
        Inbox(String life){this.life=life;}
    }
    private static Inbox inbox(ServerPlayer player){
        var connection=player.connection.getConnection();
        var life=LifeEpochs.of(connection,player);var inbox=inboxes.get(connection);
        if(inbox==null||!inbox.life.equals(life)){inbox=new Inbox(life);inboxes.put(connection,inbox);}
        return inbox;
    }
    static void install(){
        NeoForge.EVENT_BUS.addListener(EventPriority.LOWEST,false,(LivingDamageEvent.Post event)->{
            if(!(event.getSource().getDirectEntity() instanceof ServerPlayer player)
                ||!event.getSource().is(DamageTypes.PLAYER_ATTACK)
                ||!Float.isFinite(event.getNewDamage())||event.getNewDamage()<=0)return;
            record(player,Map.of("kind","attack_damage","target_uuid",event.getEntity().getUUID().toString(),
                "target_entity_id",event.getEntity().getId(),"damage",event.getNewDamage()));
        });
        NeoForge.EVENT_BUS.addListener(EventPriority.LOWEST,false,(LivingEntityUseItemEvent.Finish event)->{
            if(!(event.getEntity() instanceof ServerPlayer player))return;
            record(player,Map.of("kind","item_use_finished",
                "item",BuiltInRegistries.ITEM.getKey(event.getItem().getItem()).toString(),
                "count_before",event.getItem().getCount(),
                "result_item",BuiltInRegistries.ITEM.getKey(event.getResultStack().getItem()).toString(),
                "result_count",event.getResultStack().getCount(),
                "hand",player.getUsedItemHand().name()));
        });
    }
    private static void record(ServerPlayer player,Map<String,Object> effect){
        var service=SdkService.active;
        if(service==null||!service.owns(player)||!player.server.isSameThread())return;
        synchronized(inboxes){
            var inbox=inbox(player);
            var row=new LinkedHashMap<>(effect);
            row.put("sequence",++inbox.sequence);row.put("actor_uuid",player.getUUID().toString());
            row.put("dimension",player.serverLevel().dimension().location().toString());
            row.put("tick",player.server.getTickCount());inbox.rows.addLast(Map.copyOf(row));
            while(inbox.rows.size()>64)inbox.rows.removeFirst();
        }
    }
    static Object snapshot(ServerPlayer player){
        synchronized(inboxes){
            var inbox=inbox(player);
            return Map.of("format","native-action-effects-v1","epoch",inbox.epoch,
                "last_sequence",inbox.sequence,"events",List.copyOf(inbox.rows));
        }
    }
    private NativeEffects(){}
}
