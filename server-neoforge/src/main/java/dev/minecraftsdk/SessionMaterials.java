package dev.minecraftsdk;

import java.util.HashSet;
import java.util.Set;
import java.util.UUID;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.entity.player.Inventory;

/** Normal menu removal, before disconnect marks the player or saves its NBT.
 * Runs on the server thread; no HTTP/gameplay control operation is introduced. */
public final class SessionMaterials {
    private SessionMaterials(){}
    private static final Set<UUID> closing=new HashSet<>();
    public static void close(SdkService service,ServerPlayer player){
        UUID uuid=player.getUUID();
        if(!service.owns(player)||player.hasDisconnected()||!closing.add(uuid))return;
        try{
            // World-menu implementations retain their normal removal semantics
            // (including persistent mod workbenches); do not extract their slots.
            if(player.containerMenu!=player.inventoryMenu)player.doCloseContainer();
            // A bot can have personal crafting inputs behind another open menu.
            boolean personal=!player.inventoryMenu.getCarried().isEmpty()
                ||player.inventoryMenu.slots.stream().anyMatch(slot->!(slot.container instanceof Inventory)&&slot.hasItem());
            if(personal)player.doCloseContainer();
        }finally{closing.remove(uuid);}
    }
}
