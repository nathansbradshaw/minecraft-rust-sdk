package dev.minecraftsdk;

/** Pure-JDK regression: real 1.21.1 respawn reuses connection and entity ID. */
public final class LifeEpochsTest {
    private record Actor(int entityId) {}
    private static void check(boolean condition,String reason){if(!condition)throw new AssertionError(reason);}
    public static void main(String[] args){
        var connection=new Object();var before=new Actor(17);var after=new Actor(17);
        check(before.equals(after),"fixture must have equal reused numeric IDs");
        var first=LifeEpochs.of(connection,before);
        check(first.equals(LifeEpochs.of(connection,before)),"observations within one life must be stable");
        var next=LifeEpochs.of(connection,after);
        check(!first.equals(next),"replacement player with equal ID must change life epoch");
        check(next.equals(LifeEpochs.of(connection,after)),"new life must not churn each observation");
        check(!next.equals(LifeEpochs.of(new Object(),after)),"another connection must have an independent scope");
    }
}
